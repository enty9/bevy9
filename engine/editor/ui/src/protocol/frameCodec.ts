import {
  FRAME_HEADER_BYTES,
  PROTOCOL_VERSION,
  type DecodedViewportFrame,
} from "./types";

const MAGIC_B = 0x42; // 'B'
const MAGIC_V = 0x56; // 'V'
const MAGIC_9 = 0x39; // '9'
const MAGIC_F = 0x46; // 'F'
const PIXEL_FORMAT_RGBA8_SRGB = 1;

/**
 * Decodes a binary BV9F viewport frame packet into a header and zero-copy RGBA8 pixel slice.
 */
export function decodeViewportFrame(buffer: ArrayBuffer): DecodedViewportFrame | null {
  if (buffer.byteLength < FRAME_HEADER_BYTES) {
    return null;
  }

  const view = new DataView(buffer);
  if (
    view.getUint8(0) !== MAGIC_B ||
    view.getUint8(1) !== MAGIC_V ||
    view.getUint8(2) !== MAGIC_9 ||
    view.getUint8(3) !== MAGIC_F
  ) {
    return null;
  }

  const protocolVersion = view.getUint32(4, true);
  if (protocolVersion !== PROTOCOL_VERSION) {
    return null;
  }

  const frameId = Number(view.getBigUint64(8, true));
  const width = view.getUint32(16, true);
  const height = view.getUint32(20, true);
  const format = view.getUint8(24);

  if (format !== PIXEL_FORMAT_RGBA8_SRGB || width === 0 || height === 0) {
    return null;
  }

  const expectedPixels = width * height * 4;
  if (buffer.byteLength !== FRAME_HEADER_BYTES + expectedPixels) {
    return null;
  }

  const pixels = new Uint8ClampedArray(buffer, FRAME_HEADER_BYTES, expectedPixels);

  return {
    header: {
      protocol_version: protocolVersion,
      frame_id: frameId,
      width,
      height,
      format,
      payload_len: expectedPixels,
    },
    pixels,
  };
}
