// Camera lens selection heuristics (issue #6).
//
// getUserMedia({ facingMode: "environment" }) lets the browser pick ANY
// rear camera — on multi-lens phones that is sometimes the ultra-wide,
// whose strong distortion is exactly what the self-calibrating pipeline
// should not have to fight. These pure functions pick the main (1x) rear
// lens from an enumerateDevices() list where labels allow it, and build
// the zoom-lock constraint where the track supports zoom.
//
// Honest limits (documented fallback behaviour):
// - Device labels are only exposed AFTER a getUserMedia permission grant,
//   so the flow is: open with facingMode first, then enumerate and switch
//   if the labels identify a better lens. Before a grant (or in browsers
//   that never label, e.g. Firefox with strict privacy settings) there is
//   nothing to pick from — `pickMainRearCamera` returns null and the
//   facingMode stream is kept as-is.
// - iOS Safari exposes one virtual "Back Camera" device and does its own
//   lens switching; the pick is a no-op there, which is fine (the virtual
//   device defaults to the main lens at zoom 1x).
// - `zoom` is a non-standard MediaTrack capability (Chrome on Android,
//   mostly). Where absent, no constraint is applied and the page says so
//   rather than pretending.

/** Labels that identify a rear-facing camera. */
const REAR = /\bback\b|\brear\b|facing back|environment/i;
/** Labels that identify a front-facing camera (never acceptable). */
const FRONT = /\bfront\b|\buser\b|selfie|facing front/i;
/**
 * Labels of auxiliary rear lenses to avoid. Plain "wide" is deliberately
 * NOT matched: iOS names the main-lens pair "Back Dual Wide Camera" and
 * the main sensor itself is the "wide" camera in vendor speak — only
 * "ultra wide" (any spacing) is the distorted one.
 */
const AUX = /ultra[\s-]?wide|tele|zoom|macro|depth|infrared|fish[\s-]?eye|thermal/i;

/**
 * Pick the main (1x) rear camera from an enumerateDevices() list.
 *
 * @param {Array<{deviceId: string, kind: string, label: string}>} devices
 * @returns the chosen device info, or null when the labels don't allow a
 *          confident pick (caller keeps its facingMode stream unchanged).
 */
export function pickMainRearCamera(devices) {
  const labelled = (devices ?? []).filter(
    (d) => d.kind === "videoinput" && d.deviceId && d.label,
  );
  const rear = labelled.filter((d) => REAR.test(d.label) && !FRONT.test(d.label));
  if (rear.length === 0) {
    return null; // no labels, or none identifying a rear lens: can't do better
  }
  const main = rear.filter((d) => !AUX.test(d.label));
  // If every rear label looks auxiliary the phone probably words its main
  // lens oddly — picking the least-bad rear lens still beats the front one.
  const pool = main.length > 0 ? main : rear;
  // Tie-breaks among remaining candidates:
  // - Android labels its sensors "camera2 N, facing back"; the main sensor
  //   is (essentially always) the lowest N. The LAST number in the label is
  //   the sensor index (the first would match the "2" in "camera2").
  // - iOS labels carry no index; the main lens has the shortest label
  //   ("Back Camera" vs "Back Dual Camera" vs "Back Triple Camera").
  const score = (d) => {
    const nums = d.label.match(/\d+/g);
    return [nums ? Number(nums[nums.length - 1]) : Number.MAX_SAFE_INTEGER, d.label.length];
  };
  return pool
    .slice()
    .sort((a, b) => {
      const sa = score(a);
      const sb = score(b);
      return sa[0] - sb[0] || sa[1] - sb[1];
    })[0];
}

/**
 * Build the applyConstraints() advanced entry that locks zoom at 1.0 (or
 * the nearest value the camera's zoom range allows).
 *
 * @param capabilities the track's getCapabilities() result, or undefined
 *        where the API itself is missing.
 * @returns {{zoom: number} | null} null when zoom is not controllable.
 */
export function zoomLockConstraint(capabilities) {
  if (!capabilities || !("zoom" in capabilities)) {
    return null;
  }
  const z = capabilities.zoom;
  const min = typeof z?.min === "number" ? z.min : 1.0;
  const max = typeof z?.max === "number" ? z.max : 1.0;
  if (!(Number.isFinite(min) && Number.isFinite(max)) || min > max) {
    return null;
  }
  return { zoom: Math.min(Math.max(1.0, min), max) };
}
