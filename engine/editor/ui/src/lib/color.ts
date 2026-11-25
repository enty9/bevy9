export interface RgbaChannels {
  red: number;
  green: number;
  blue: number;
  alpha: number;
}

export interface DecodedColorValue {
  isEnumColor: boolean;
  variantName: "Srgba" | "LinearRgba";
  isLinear: boolean;
  rgba: RgbaChannels;
}

function clamp01(value: number): number {
  return Math.max(0, Math.min(1, Number.isFinite(value) ? value : 0));
}

export function linearToSrgbChannel(linear: number): number {
  const c = clamp01(linear);
  if (c <= 0.0031308) {
    return c * 12.92;
  }
  return 1.055 * Math.pow(c, 1 / 2.4) - 0.055;
}

export function srgbToLinearChannel(srgb: number): number {
  const c = clamp01(srgb);
  if (c <= 0.04045) {
    return c / 12.92;
  }
  return Math.pow((c + 0.055) / 1.055, 2.4);
}

function parseChannels(raw: unknown): RgbaChannels {
  if (raw && typeof raw === "object") {
    const rec = raw as Record<string, unknown>;
    return {
      red: typeof rec.red === "number" ? rec.red : 1,
      green: typeof rec.green === "number" ? rec.green : 1,
      blue: typeof rec.blue === "number" ? rec.blue : 1,
      alpha: typeof rec.alpha === "number" ? rec.alpha : 1,
    };
  }
  return { red: 1, green: 1, blue: 1, alpha: 1 };
}

export function extractColorInfo(
  value: unknown,
  typePath?: string,
): DecodedColorValue {
  if (typePath === "bevy_color::linear_rgba::LinearRgba") {
    return {
      isEnumColor: false,
      variantName: "LinearRgba",
      isLinear: true,
      rgba: parseChannels(value),
    };
  }

  if (typePath === "bevy_color::srgba::Srgba") {
    return {
      isEnumColor: false,
      variantName: "Srgba",
      isLinear: false,
      rgba: parseChannels(value),
    };
  }

  if (value && typeof value === "object") {
    const rec = value as Record<string, unknown>;
    if ("LinearRgba" in rec) {
      return {
        isEnumColor: true,
        variantName: "LinearRgba",
        isLinear: true,
        rgba: parseChannels(rec.LinearRgba),
      };
    }
    if ("Srgba" in rec) {
      return {
        isEnumColor: true,
        variantName: "Srgba",
        isLinear: false,
        rgba: parseChannels(rec.Srgba),
      };
    }
  }

  return {
    isEnumColor: true,
    variantName: "Srgba",
    isLinear: false,
    rgba: { red: 1, green: 1, blue: 1, alpha: 1 },
  };
}

export function buildColorValue(
  info: DecodedColorValue,
  nextRgba: RgbaChannels,
): unknown {
  if (!info.isEnumColor) {
    return nextRgba;
  }
  return {
    [info.variantName]: nextRgba,
  };
}

export function rgbaToHex(rgba: RgbaChannels, isLinear: boolean): string {
  const r = isLinear ? linearToSrgbChannel(rgba.red) : clamp01(rgba.red);
  const g = isLinear ? linearToSrgbChannel(rgba.green) : clamp01(rgba.green);
  const b = isLinear ? linearToSrgbChannel(rgba.blue) : clamp01(rgba.blue);
  const toByte = (v: number) =>
    Math.round(v * 255)
      .toString(16)
      .padStart(2, "0");
  return `#${toByte(r)}${toByte(g)}${toByte(b)}`;
}

export function hexToRgba(
  hex: string,
  alpha: number,
  isLinear: boolean,
): RgbaChannels {
  const clean = hex.replace("#", "");
  if (clean.length !== 6) {
    return { red: 1, green: 1, blue: 1, alpha };
  }
  const rSrgb = parseInt(clean.slice(0, 2), 16) / 255;
  const gSrgb = parseInt(clean.slice(2, 4), 16) / 255;
  const bSrgb = parseInt(clean.slice(4, 6), 16) / 255;

  if (isLinear) {
    return {
      red: Number(srgbToLinearChannel(rSrgb).toFixed(4)),
      green: Number(srgbToLinearChannel(gSrgb).toFixed(4)),
      blue: Number(srgbToLinearChannel(bSrgb).toFixed(4)),
      alpha,
    };
  }
  return {
    red: Number(rSrgb.toFixed(4)),
    green: Number(gSrgb.toFixed(4)),
    blue: Number(bSrgb.toFixed(4)),
    alpha,
  };
}
