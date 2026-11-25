function cleanZero(value: number): number {
  const rounded = Math.round(value * 1000) / 1000;
  return Object.is(rounded, -0) ? 0 : rounded;
}

/**
 * Converts a quaternion `[x, y, z, w]` to extrinsic XYZ Euler angles in degrees.
 */
export function quatToEulerDeg(
  quat: [number, number, number, number],
): [number, number, number] {
  const [x, y, z, w] = quat;

  const sinrCosp = 2 * (w * x + y * z);
  const cosrCosp = 1 - 2 * (x * x + y * y);
  const rx = Math.atan2(sinrCosp, cosrCosp);

  const sinp = 2 * (w * y - z * x);
  const ry =
    Math.abs(sinp) >= 1 ? Math.sign(sinp) * (Math.PI / 2) : Math.asin(sinp);

  const sinyCosp = 2 * (w * z + x * y);
  const cosyCosp = 1 - 2 * (y * y + z * z);
  const rz = Math.atan2(sinyCosp, cosyCosp);

  const radToDeg = 180 / Math.PI;
  return [
    cleanZero(rx * radToDeg),
    cleanZero(ry * radToDeg),
    cleanZero(rz * radToDeg),
  ];
}

/**
 * Converts extrinsic XYZ Euler angles in degrees to a normalized `[x, y, z, w]` quaternion.
 */
export function eulerDegToQuat(
  eulerDeg: [number, number, number],
): [number, number, number, number] {
  const degToRad = Math.PI / 180;
  const hx = eulerDeg[0] * degToRad * 0.5;
  const hy = eulerDeg[1] * degToRad * 0.5;
  const hz = eulerDeg[2] * degToRad * 0.5;

  const sx = Math.sin(hx);
  const cx = Math.cos(hx);
  const sy = Math.sin(hy);
  const cy = Math.cos(hy);
  const sz = Math.sin(hz);
  const cz = Math.cos(hz);

  const x = sx * cy * cz - cx * sy * sz;
  const y = cx * sy * cz + sx * cy * sz;
  const z = cx * cy * sz - sx * sy * cz;
  const w = cx * cy * cz + sx * sy * sz;

  const len = Math.hypot(x, y, z, w) || 1;
  return [x / len, y / len, z / len, w / len];
}
