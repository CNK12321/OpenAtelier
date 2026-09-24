"""OpenAtelier's bridge to CoTracker (facebookresearch/co-tracker).

Written into the tracker's folder and run with the tracker's own Python (see
`engine.rs`); nothing is installed anywhere else. Like the caption bridge it talks in
JSON lines on stdout, one object per line with a "type", and leaves stderr to progress
("NN%" lines). PyTorch's hub keeps CoTracker's code and weights under TORCH_HOME (the
tracker's folder).

    python oa_track.py download
    python oa_track.py track --frames f.rgb --width W --height H --frame 12 --x 100 --y 80

The frames are raw RGB (8 bits a channel), one after another, as ffmpeg writes them.

It uses CoTracker's *online* model: it walks the footage in overlapping windows of 16
frames, so memory stays flat however long the stretch is, and after each window it
reports how far it's got and where the point is (`at` lines) — the editor draws the
track growing and moves the playhead along with it. Lines it writes:

    {"type": "device", "name": "NVIDIA GeForce RTX 3070"}
    {"type": "at", "frame": 57, "x": 211.4, "y": 98.0}
    {"type": "track", "points": [[x, y, seen], ...]}      one per frame, at the end
"""

import argparse
import json
import math
import os
import sys

# Apple GPUs lack a few operations CoTracker uses: let those fall back to the CPU.
os.environ.setdefault("PYTORCH_ENABLE_MPS_FALLBACK", "1")

REPO = "facebookresearch/co-tracker"
MODEL = "cotracker3_online"


def emit(**event):
    print(json.dumps(event), flush=True)


def progress(fraction):
    print(f"{min(max(fraction, 0.0), 1.0) * 100:.1f}%", file=sys.stderr, flush=True)


def load():
    import torch

    # Once downloaded, load from the local copy: no network, no GitHub rate limits.
    local = os.path.join(torch.hub.get_dir(), "facebookresearch_co-tracker_main")
    if os.path.isdir(local):
        return torch.hub.load(local, MODEL, source="local")
    return torch.hub.load(REPO, MODEL, trust_repo=True)


def download(_args):
    load()
    emit(type="done", path="")


def pick_device(torch):
    """The fastest place it can run: an NVIDIA card (in half precision), Apple's GPU, or
    the CPU (PyTorch spreads the work over every core itself)."""
    if torch.cuda.is_available():
        return "cuda", torch.cuda.get_device_name(0)
    if getattr(torch.backends, "mps", None) and torch.backends.mps.is_available():
        return "mps", "Apple GPU"
    return "cpu", f"CPU, {torch.get_num_threads()} threads"


def windows(n, step):
    """The online model's windows over n frames (n > step), as (start, end, first):
    what CoTracker's own online demo feeds it. The first only sets the model up."""
    out = []
    first = True
    for i in range(step, n, step):
        out.append((max(0, i - 2 * step), i, first))
        first = False
    i = n - 1
    out.append((n - (i % step) - step - 1, n, first))
    return out


def track(args):
    import contextlib

    import numpy as np
    import torch

    size = args.width * args.height * 3
    count = os.path.getsize(args.frames) // size
    if count < 2:
        raise RuntimeError("not enough frames to follow a point through")
    # Read from disk as needed rather than all at once.
    frames = np.memmap(args.frames, dtype=np.uint8, mode="r", shape=(count, args.height, args.width, 3))
    progress(0.02)

    device, name = pick_device(torch)
    emit(type="device", name=name)
    model = load().to(device).eval()
    step = model.step
    progress(0.08)
    half = torch.autocast(device_type="cuda", dtype=torch.float16) if device == "cuda" else contextlib.nullcontext()

    frame = min(max(args.frame, 0), count - 1)
    # Forward from the start point, then backward only if there are frames before it —
    # each pass over just its own frames.
    passes = [(frames[frame:], lambda k: frame + k)]
    if frame > 0:
        passes.append((frames[: frame + 1][::-1], lambda k: frame - k))
    work = sum(len(windows(max(len(c), step + 1), step)) for c, _ in passes)
    done = 0

    def follow(clip, index_of):
        nonlocal done
        length = len(clip)
        # Too short for one window: repeat the last frame (the extra is dropped).
        if length <= step:
            clip = np.concatenate([np.asarray(clip), np.repeat(np.asarray(clip[-1:]), step + 1 - length, axis=0)])
        n = len(clip)
        queries = torch.tensor([[[0.0, args.x, args.y]]], device=device)
        tracks = visible = None
        for start, end, first in windows(n, step):
            chunk = torch.from_numpy(np.ascontiguousarray(clip[start:end])).to(device).permute(0, 3, 1, 2)[None].float()
            with torch.inference_mode(), half:
                out = model(chunk, is_first_step=first, queries=queries if first else None, add_support_grid=True)
            done += 1
            progress(0.08 + 0.9 * done / work)
            if not first:
                tracks, visible = out
                newest = min(end, length) - 1
                p = tracks[0, newest, 0].float().cpu()
                if math.isfinite(float(p[0])) and math.isfinite(float(p[1])):
                    emit(type="at", frame=index_of(newest), x=float(p[0]), y=float(p[1]))
        tracks, visible = tracks[0, :length, 0].float().cpu(), visible[0, :length, 0].cpu()
        # A lost frame (not a number) keeps the last place it was seen: NaN isn't JSON.
        out, last = [], [args.x, args.y]
        for i in range(len(tracks)):
            x, y = float(tracks[i, 0]), float(tracks[i, 1])
            ok = math.isfinite(x) and math.isfinite(y)
            last = [x, y] if ok else last
            out.append([last[0], last[1], bool(visible[i]) and ok])
        return out

    points = follow(*passes[0])
    if len(passes) > 1:
        back = follow(*passes[1])
        # back[k] is frame - k: the frames before the start, in order, then forward.
        points = back[:0:-1] + points
    progress(1.0)
    emit(type="track", points=points)


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("download")
    t = sub.add_parser("track")
    t.add_argument("--frames", required=True)
    t.add_argument("--width", type=int, required=True)
    t.add_argument("--height", type=int, required=True)
    t.add_argument("--frame", type=int, required=True)
    t.add_argument("--x", type=float, required=True)
    t.add_argument("--y", type=float, required=True)
    args = parser.parse_args()
    try:
        {"download": download, "track": track}[args.command](args)
    except Exception as e:  # reported to the editor, which shows it
        emit(type="error", message=f"{type(e).__name__}: {e}")
        sys.exit(1)


if __name__ == "__main__":
    main()
