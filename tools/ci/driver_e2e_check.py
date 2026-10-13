"""End-to-end driver ring check through bio-live and VB-CABLE.

Starts bio-live on the Spatial SoftReceiver control ring, plays a distinct shared-mode tone
into each of the three endpoints (primary, music, passthrough), captures CABLE
Output, then requires every tone to stand at least 20 dB above nearby spectral
bins. A 20 dB margin is
large enough to reject leakage/noise while allowing BRIR gain and cancellation.
"""

import argparse
import json
import os
import subprocess
import sys
import threading
import time

import numpy as np

from endpoints import enumerate_endpoints, find_speaker

# One tone per endpoint. 2250 Hz is no harmonic of the others, and their
# difference products (500, 750, 1250 Hz) fall on none of the three.
TONES = {0: 1000.0, 1: 1500.0, 2: 2250.0}


def tone_margin_db(signal, rate, frequency, guard_hz=40.0):
    """Returns (tone dBFS, local-noise dBFS, margin dB) for a mono signal."""
    if len(signal) < 2:
        return -200.0, -200.0, 0.0
    window = np.hanning(len(signal))
    spectrum = np.abs(np.fft.rfft(signal * window)) * 2.0 / window.sum()
    frequencies = np.fft.rfftfreq(len(signal), 1.0 / rate)
    peak = spectrum[np.argmin(np.abs(frequencies - frequency))]
    neighborhood = (np.abs(frequencies - frequency) >= guard_hz) & (
        np.abs(frequencies - frequency) <= 300.0
    )
    noise = np.median(spectrum[neighborhood]) if np.any(neighborhood) else 1e-10
    tone_db = 20.0 * np.log10(max(float(peak), 1e-10))
    noise_db = 20.0 * np.log10(max(float(noise), 1e-10))
    return tone_db, noise_db, tone_db - noise_db


LOSS_COUNTERS = (
    "underruns_while_streaming",
    "output_reconnects",
    "output_cadence_faults",
    "dropped_frames",
    "lane_underrun_frames",
    "output_overflow_frames",
    "dropped_events",
)


LATENCY_CEILING_MS = 1_000


def check_report(report):
    """Returns failures for missing state and discontinuities in a bio-live report."""
    problems = []
    for key in LOSS_COUNTERS:
        if key not in report:
            problems.append(f"report is missing {key}")
        elif report[key] != 0:
            problems.append(f"{key} is {report[key]}")
    if report.get("output_fell_back"):
        problems.append("exclusive output fell back to shared mode")
    # As loopback_check.LATENCY_CEILING_MS: no chain is this slow, so such a
    # sample is a block stamped with a source time from another clock.
    for key in ("latency_p50_ms", "latency_p99_ms"):
        value = report.get(key)
        if isinstance(value, (int, float)) and value > LATENCY_CEILING_MS:
            problems.append(f"{key} is {value} ms, above {LATENCY_CEILING_MS} ms")

    endpoints = report.get("endpoints")
    if not isinstance(endpoints, list) or len(endpoints) != 4:
        problems.append("report endpoints must contain exactly four entries")
    else:
        for expected, endpoint in enumerate(endpoints):
            for key in ("endpoint", "format", "epoch", "unsupported"):
                if key not in endpoint:
                    problems.append(f"report endpoint {expected} is missing {key}")
            if endpoint.get("endpoint") != expected:
                problems.append(
                    f"report endpoint slot {expected} names {endpoint.get('endpoint')}"
                )
            if endpoint.get("unsupported") is not None:
                problems.append(
                    f"endpoint {expected} has unsupported format: {endpoint['unsupported']}"
                )

    driver = report.get("driver")
    if driver is None:
        problems.append("report is missing driver statistics")
    elif not isinstance(driver.get("endpoints"), list):
        problems.append("driver statistics are missing endpoints")
    else:
        for endpoint in driver["endpoints"]:
            if "lost_bytes" not in endpoint:
                problems.append(
                    f"driver endpoint {endpoint.get('endpoint')} is missing lost_bytes"
                )
            elif endpoint["lost_bytes"] != 0:
                problems.append(
                    f"driver endpoint {endpoint.get('endpoint')} lost {endpoint['lost_bytes']} bytes"
                )
    return problems


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bio-live", required=True)
    parser.add_argument("--profile", required=True)
    parser.add_argument("--seconds", type=float, default=8.0)
    parser.add_argument("--render", default="VB-Audio Virtual Cable")
    parser.add_argument("--capture", default="CABLE Output")
    parser.add_argument("--report", required=True)
    parser.add_argument("--summary", required=True)
    parser.add_argument("--min-margin-db", type=float, default=20.0)
    parser.add_argument(
        "--record",
        help="also have bio-live write the READ stream it captured to this dump, "
        "so a failure can be replayed offline (bio-replay render)",
    )
    args = parser.parse_args()

    from loopback_check import binary_fromstring
    np.fromstring = binary_fromstring
    import soundcard as sc

    rows = enumerate_endpoints()
    speakers = {endpoint: find_speaker(sc, endpoint, rows) for endpoint in TONES}
    if not all(speakers.values()):
        raise SystemExit(f"Spatial SoftReceiver endpoints missing: {speakers}")
    microphone = sc.get_microphone(args.capture)
    rate = 48_000
    command = [
        args.bio_live,
        "play",
        "--source",
        "driver",
        "--output",
        args.render,
        "--profile",
        args.profile,
        "--seconds",
        str(args.seconds),
        "--report",
        args.report,
        "--shared",
    ]
    if args.record:
        command += ["--record", args.record]
    chunks = []
    errors = []

    def play(endpoint):
        try:
            t = np.arange(int((args.seconds - 1.0) * rate)) / rate
            tone = (0.25 * np.sin(2 * np.pi * TONES[endpoint] * t)).astype(np.float32)
            stereo = np.stack([tone, tone], axis=1)
            with speakers[endpoint].player(
                samplerate=rate, channels=2, blocksize=rate // 100
            ) as player:
                player.play(stereo)
        except Exception as error:  # CI must preserve the worker failure in its summary.
            errors.append(f"endpoint {endpoint}: {error}")

    with microphone.recorder(samplerate=rate, channels=2, blocksize=rate // 100) as recorder:
        chunks.append(recorder.record(numframes=rate // 2))
        live = subprocess.Popen(command, stderr=subprocess.PIPE, text=True)
        time.sleep(0.5)
        players = [threading.Thread(target=play, args=(endpoint,)) for endpoint in TONES]
        for player in players:
            player.start()
        deadline = time.monotonic() + args.seconds + 30.0
        while live.poll() is None and time.monotonic() < deadline:
            chunks.append(recorder.record(numframes=rate // 20))
        if live.poll() is None:
            live.kill()
            errors.append("bio-live timed out")
        for player in players:
            player.join()
        chunks.append(recorder.record(numframes=rate // 2))

    stderr = live.stderr.read() if live.stderr else ""
    if live.returncode:
        errors.append(f"bio-live exited with {live.returncode}: {stderr}")
    captured = np.concatenate(chunks).astype(np.float32)
    mono = captured.mean(axis=1)
    tones = {}
    for endpoint, frequency in TONES.items():
        level, noise, margin = tone_margin_db(mono, rate, frequency)
        tones[str(endpoint)] = {
            "frequency_hz": frequency,
            "tone_dbfs": round(level, 2),
            "noise_dbfs": round(noise, 2),
            "margin_db": round(margin, 2),
        }
        if margin < args.min_margin_db:
            errors.append(f"{frequency:.0f} Hz margin is {margin:.1f} dB")

    report = {}
    if os.path.exists(args.report):
        with open(args.report, encoding="utf-8") as source:
            report = json.load(source)
        errors.extend(check_report(report))
    else:
        errors.append("bio-live did not write its report")
    summary = {
        "render_endpoints": rows,
        "tones": tones,
        "clock_strategy": report.get("clock_strategy"),
        "report": report,
        "problems": errors,
    }
    with open(args.summary, "w", encoding="utf-8") as output:
        json.dump(summary, output, indent=2)
    print(json.dumps(summary, indent=2))
    step = os.environ.get("GITHUB_STEP_SUMMARY")
    if step:
        with open(step, "a", encoding="utf-8") as output:
            output.write("\n### Driver ring to VB-CABLE\n\n")
            output.write(f"Clock strategy: {summary['clock_strategy']}\n\n")
            for tone in tones.values():
                output.write(
                    f"- {tone['frequency_hz']:.0f} Hz: {tone['tone_dbfs']} dBFS, "
                    f"{tone['margin_db']} dB above local noise\n"
                )
            output.write("\n" + ("FAIL: " + "; ".join(errors) if errors else "ok") + "\n")
    for error in errors:
        print(error, file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
