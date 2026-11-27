"""Generate a short, original test clip for the Content Browser (no extra modules)."""

import argparse
import math
import struct
import wave
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="Destination .wav path (must not exist)")
    args = parser.parse_args()
    rate = 24_000
    frames = bytearray()
    for note in (261.626, 329.628, 391.995, 523.251) * 2:
        for sample in range(rate):
            t = sample / rate
            envelope = min(t / 0.015, 1.0) * max(1.0 - t / 0.9, 0.0) ** 2
            signal = math.sin(math.tau * note * t) + 0.15 * math.sin(math.tau * 3 * note * t)
            frames += struct.pack("<h", int(5000 * envelope * signal))
    with args.output.open("xb") as stream, wave.open(stream, "wb") as clip:
        clip.setparams((1, 2, rate, 0, "NONE", "not compressed"))
        clip.writeframes(frames)


if __name__ == "__main__":
    main()
