"""Multichannel VB-CABLE loopback check for the Windows audio CI job."""

import argparse
import json
import math
import struct
import subprocess
import sys
import time
from pathlib import Path

import numpy as np
import soundfile as sf

from loopback_check import compare, report_problems

RATE = 48_000
LAYOUTS = {8: (0x63F, ["L", "R", "C", "LFE", "Lrs", "Rrs", "Lss", "Rss"]),
           6: (0x3F, ["L", "R", "C", "LFE", "Lrs", "Rrs"]),
           4: (0x33, ["L", "R", "Lrs", "Rrs"])}


def choose_channels(render, capture, forced=None):
    """Largest 4/6/8 channel layout accepted exclusively at both endpoints."""
    def supported(probe, channels):
        return any(row["channels"] == channels and row["exclusive"] for row in probe["layouts"])
    choices = [forced] if forced is not None else [8, 6, 4]
    return next((n for n in choices if n in LAYOUTS and supported(render, n) and supported(capture, n)), None)


def profile_text(channels):
    mask, names = LAYOUTS[channels]
    lines = ["kind = \"speakers\"", "name = \"CI channel assignment\"",
             f"device_channels = {channels}", f"channel_mask = {mask}",
             "lfe_gain_db = 0.0", "limiter_ceiling_dbfs = 0.0", ""]
    for channel, name in enumerate(names):
        if name == "LFE":
            lines += ["[[subwoofer]]", f"channel = {channel}", ""]
        else:
            lines += ["[[speaker]]", f"name = \"{name}\"", f"channel = {channel}",
                      "range = \"full\"", ""]
    return "\n".join(lines)


def tone_frames(channels, seconds=4.0):
    t = np.arange(round(seconds * RATE), dtype=np.float64) / RATE
    frequencies = np.array([300.0 * 1.25 ** k for k in range(channels)])
    return (0.1 * np.sin(2 * np.pi * t[:, None] * frequencies[None, :])).astype(np.float32)


def _record(kind, epoch, byte_pos, qpc, payload=b""):
    header = struct.pack("<HHIQQQIHH", kind, 0, epoch, byte_pos, qpc, 0, len(payload), 0, 0)
    return header + payload + b"\0" * ((-len(payload)) % 8)


def make_dump(path, frames, channel_mask):
    """Write a bio-abi format-1 dump containing float32 interleaved PCM."""
    channels = frames.shape[1]
    block = channels * 4
    wave = struct.pack("<HHIIHHHHI16s", 0xFFFE, channels, RATE, RATE * block, block,
                       32, 22, 32, channel_mask,
                       bytes.fromhex("0300000000001000800000aa00389b71"))
    stream = bytearray()
    stream += _record(3, 1, 0, 0)
    stream += _record(2, 1, 0, 0, wave)
    payload = frames.astype("<f4", copy=False).tobytes()
    period_bytes = RATE // 100 * block
    for at in range(0, len(payload), period_bytes):
        chunk = payload[at:at + period_bytes]
        qpc = ((at + len(chunk)) // block) * 10_000_000 // RATE
        stream += _record(1, 1, at, qpc, chunk)
    stream += _record(4, 1, len(payload), frames.shape[0] * 10_000_000 // RATE)
    header = struct.pack("<8sIIQII", b"BIODUMP\0", 1, 32, 10_000_000, 1, 0)
    Path(path).write_bytes(header + stream)


# How far a tone may sit from its nominal frequency: the live clock's ASRC
# shifts every tone by its correction (the PI limit is 2000 ppm), and the
# tones are 25 % apart.
TONE_TOLERANCE = 0.01
# Trimmed from each end of the sounding part: the boundary fades and the
# capture's start are not the steady tone.
EDGE_S = 0.1


def steady_span(captured, rate):
    """Frames where the recording carries the tones, minus the edges."""
    hop = rate // 100
    usable = len(captured) // hop * hop
    if usable == 0:
        return 0, len(captured)
    energy = (captured[:usable].astype(np.float64) ** 2).sum(axis=1)
    envelope = energy.reshape(-1, hop).mean(axis=1)
    active = np.flatnonzero(envelope > 0.25 * envelope.max(initial=0.0))
    if active.size == 0:
        return 0, len(captured)
    edge = int(EDGE_S * rate)
    start, end = active[0] * hop + edge, (active[-1] + 1) * hop - edge
    if end - start < rate // 4:
        return 0, len(captured)
    return start, end


def assignment_matrix(captured, rate, frequencies):
    """Power of reference tone k (row) in captured channel j (column).

    Each entry sums a band of TONE_TOLERANCE around the tone over the steady
    part of the recording, so a clock correction that moves the tone off its
    nominal bin is still counted as that tone, and a tone that starts or stops
    inside the window does not smear into its neighbours' bands.
    """
    start, end = steady_span(captured, rate)
    segment = captured[start:end].astype(np.float64)
    window = np.hanning(len(segment))[:, None]
    power = np.abs(np.fft.rfft(segment * window, axis=0)) ** 2
    bin_hz = rate / len(segment)
    rows = []
    for frequency in frequencies:
        half = max(TONE_TOLERANCE * frequency / bin_hz, 4.0)
        lo = max(0, int(np.floor(frequency / bin_hz - half)))
        hi = min(power.shape[0], int(np.ceil(frequency / bin_hz + half)) + 1)
        rows.append(power[lo:hi].sum(axis=0))
    return np.array(rows)


def assignment_problems(matrix, margin_db=40.0):
    problems = []
    for k in range(matrix.shape[0]):
        diagonal = matrix[k, k]
        others = np.delete(matrix[k], k)
        ratio_db = 10 * math.log10((diagonal + 1e-30) / (others.sum() + 1e-30))
        if ratio_db < margin_db:
            problems.append(f"reference channel {k} separation is {ratio_db:.1f} dB")
    for j in range(matrix.shape[1]):
        if matrix[:, j].max(initial=0) < 1e-12:
            problems.append(f"captured channel {j} is silent")
    return problems


def probe(command, endpoint, capture=False):
    args = [command, "--endpoint", endpoint]
    if capture:
        args.append("--capture")
    return json.loads(subprocess.check_output(args, text=True))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--format-probe", required=True)
    parser.add_argument("--cable-capture", required=True)
    parser.add_argument("--bio-live", required=True)
    parser.add_argument("--bio-replay", required=True)
    parser.add_argument("--work", required=True)
    parser.add_argument("--summary", required=True)
    parser.add_argument("--channels", type=int)
    # The cable's render endpoint is "Speakers (VB-Audio Virtual Cable)" on
    # the runners, as loopback_check.py and soak.ps1 expect.
    parser.add_argument("--render", default="VB-Audio Virtual Cable")
    parser.add_argument("--capture", default="CABLE Output")
    parser.add_argument("--seconds", type=float, default=4.0)
    # Exclusive by default: a shared stream is matrixed to the endpoint's mix
    # format, so a stereo-configured cable would fold the layout before the
    # routing check sees it. 20 ms keeps VB-CABLE's timer-driven cadence
    # (ADR 0026), as the stereo loopback does.
    parser.add_argument("--mode", choices=["exclusive", "shared"], default="exclusive")
    parser.add_argument("--period-ms", type=float, default=20.0)
    args = parser.parse_args()
    work = Path(args.work)
    work.mkdir(parents=True, exist_ok=True)
    render_probe = probe(args.format_probe, args.render)
    capture_probe = probe(args.format_probe, args.capture, True)
    channels = choose_channels(render_probe, capture_probe, args.channels)
    summary = {"selected_channels": channels, "passed": False, "mode": args.mode,
               "render_probe": render_probe, "capture_probe": capture_probe}
    if channels is None:
        summary["reason"] = "render and capture share no exclusive 4/6/8-channel layout"
        Path(args.summary).write_text(json.dumps(summary, indent=2), encoding="utf-8")
        return 2
    mask, _ = LAYOUTS[channels]
    profile = work / "multichannel-profile.toml"
    dump = work / "multichannel.biodump"
    reference = work / "multichannel-reference.wav"
    captured = work / "multichannel-captured.wav"
    report = work / "multichannel-live-report.json"
    profile.write_text(profile_text(channels), encoding="utf-8")
    frames = tone_frames(channels, args.seconds)
    make_dump(dump, frames, mask)
    subprocess.run([args.bio_replay, "render", str(dump), "--profile", str(profile),
                    "--out", str(reference)], check=True)
    capture = subprocess.Popen([args.cable_capture, "--endpoint", args.capture,
        "--seconds", str(args.seconds + 2), "--channels", str(channels), "--mask", hex(mask),
        "--exclusive", str(captured)])
    time.sleep(0.5)
    mode = ["--shared"] if args.mode == "shared" else ["--period-ms", str(args.period_ms)]
    live = subprocess.run([args.bio_live, "play", "--from-dump", str(dump), "--profile",
        str(profile), "--output", args.render, *mode,
        "--seconds", str(args.seconds + 0.5), "--report", str(report)], timeout=120)
    capture_rc = capture.wait(timeout=30)
    problems = []
    if live.returncode:
        problems.append(f"bio-live exited with {live.returncode}")
    if capture_rc:
        problems.append(f"cable_capture exited with {capture_rc}")
    ref, ref_rate = sf.read(reference, dtype="float32", always_2d=True)
    cap, cap_rate = sf.read(captured, dtype="float32", always_2d=True)
    if ref.shape[1] != channels:
        problems.append(f"reference has {ref.shape[1]} channels instead of {channels}")
    if cap.shape[1] != channels:
        problems.append(f"capture has {cap.shape[1]} channels instead of {channels}")
    if problems:
        summary.update(problems=problems, passed=False)
        Path(args.summary).write_text(json.dumps(summary, indent=2), encoding="utf-8")
        print(json.dumps(summary, indent=2))
        return 1
    correlations = []
    for channel in range(channels):
        mono_ref = np.repeat(ref[:, channel, None], 2, axis=1)
        mono_cap = np.repeat(cap[:, channel, None], 2, axis=1)
        correlation = compare(mono_ref, ref_rate, mono_cap, cap_rate, 0.95)
        correlations.append(correlation)
        if correlation["windows"] == 0 or correlation["matched"] < 0.9:
            problems.append(
                f"channel {channel} matched {correlation['matched']:.1%} "
                f"of {correlation['windows']} correlation windows"
            )
    frequencies = [300.0 * 1.25 ** k for k in range(channels)]
    matrix = assignment_matrix(cap, cap_rate, frequencies)
    problems += assignment_problems(matrix)
    figures = json.loads(report.read_text(encoding="utf-8"))
    problems += report_problems(figures, expected_mode=args.mode)
    summary.update(correlations=correlations, assignment_matrix=matrix.tolist(),
                   problems=problems, passed=not problems,
                   files={"profile": str(profile), "dump": str(dump),
                          "reference": str(reference), "captured": str(captured),
                          "report": str(report)})
    Path(args.summary).write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps(summary, indent=2))
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
