"""OpenAtelier's bridge to SAM 2 (facebookresearch/sam2), for automatic rotoscoping.

Written into the AI engine's folder and run with its own Python (see `roto.rs`), which
already has PyTorch; SAM 2's code is a copy of its repository in that folder, put on
the path here (its package is only published as source that compiles an optional CUDA
extension). Like the other bridges it talks in JSON lines on stdout, one object per
line with a "type", and leaves stderr to progress ("NN%" lines).

    python oa_roto.py check --src DIR
    python oa_roto.py segment --src DIR --checkpoint sam2.1_hiera_tiny.pt
        --config configs/sam2.1/sam2.1_hiera_t.yaml --frames f.rgb --width W --height H
        --frame 12 --points '[[x, y, 1], [x, y, 0]]' --out mattes.u8 --work DIR

The frames are raw RGB (8 bits a channel), one after another, as ffmpeg writes them;
the points are on frame `--frame`, in its pixels, 1 for "this" and 0 for "not this".
The result is one byte of coverage a pixel (SAM's probability × 255), frame after
frame, at the frames' size.

SAM 2 keeps every frame it's given in memory (about 12 MB each at its working size), so
the footage goes through it in chunks: the first starts with the clicked points, each
next one with the mask the last one ended on. Forward from the clicked frame, then
backward from it if there are frames before. Lines it writes:

    {"type": "device", "name": "NVIDIA GeForce RTX 3070"}
    {"type": "at", "frame": 57}
    {"type": "matte", "path": "...", "frames": 120, "width": 768, "height": 432}
"""

import argparse
import contextlib
import json
import os
import shutil
import sys

os.environ.setdefault("PYTORCH_ENABLE_MPS_FALLBACK", "1")

# Frames per chunk: memory for SAM 2's copy of them (≈ 12 MB each) stays under 1 GB.
CHUNK = 64


def emit(**event):
    print(json.dumps(event), flush=True)


def progress(fraction):
    print(f"{min(max(fraction, 0.0), 1.0) * 100:.1f}%", file=sys.stderr, flush=True)


def pick_device(torch):
    if torch.cuda.is_available():
        return "cuda", torch.cuda.get_device_name(0)
    if getattr(torch.backends, "mps", None) and torch.backends.mps.is_available():
        return "mps", "Apple GPU"
    return "cpu", f"CPU, {torch.get_num_threads()} threads"


def use_source(src):
    sys.path.insert(0, src)


def check(args):
    use_source(args.src)
    from sam2.build_sam import build_sam2_video_predictor  # noqa: F401

    emit(type="done", path="")


def chunks(order, size):
    """`order` in runs of `size`, each starting on the frame the last one ended on."""
    out, i = [], 0
    while True:
        run = order[i : i + size]
        out.append(run)
        if i + len(run) >= len(order) or len(run) < 2:
            return out
        i += len(run) - 1


def segment(args):
    use_source(args.src)
    import numpy as np
    import torch
    from PIL import Image
    from sam2.build_sam import build_sam2_video_predictor

    w, h = args.width, args.height
    count = os.path.getsize(args.frames) // (w * h * 3)
    if count < 1:
        raise RuntimeError("no frames to find the outline in")
    frames = np.memmap(args.frames, dtype=np.uint8, mode="r", shape=(count, h, w, 3))
    out = np.memmap(args.out, dtype=np.uint8, mode="w+", shape=(count, h, w))
    points = json.loads(args.points)
    if not points:
        raise RuntimeError("click what to follow first")
    coords = np.array([[p[0], p[1]] for p in points], dtype=np.float32)
    labels = np.array([1 if p[2] else 0 for p in points], dtype=np.int32)
    progress(0.02)

    device, name = pick_device(torch)
    emit(type="device", name=name)
    predictor = build_sam2_video_predictor(args.config, args.checkpoint, device=device)
    progress(0.06)
    fast = torch.autocast("cuda", dtype=torch.bfloat16) if device == "cuda" else contextlib.nullcontext()

    start = min(max(args.frame, 0), count - 1)
    passes = [list(range(start, count))]
    if start > 0:
        passes.append(list(range(start, -1, -1)))
    total = sum(len(p) for p in passes)
    done = 0
    folder = os.path.join(args.work, "chunk")

    for order in passes:
        carried = None
        for run in chunks(order, CHUNK):
            # SAM 2 reads a folder of JPEGs named by their order.
            shutil.rmtree(folder, ignore_errors=True)
            os.makedirs(folder)
            for k, f in enumerate(run):
                Image.fromarray(np.asarray(frames[f])).save(os.path.join(folder, f"{k:05d}.jpg"), quality=95)
            with torch.inference_mode(), fast:
                state = predictor.init_state(video_path=folder, offload_video_to_cpu=True)
                if carried is None:
                    predictor.add_new_points_or_box(state, frame_idx=0, obj_id=1, points=coords, labels=labels)
                else:
                    predictor.add_new_mask(state, frame_idx=0, obj_id=1, mask=carried)
                for k, _ids, logits in predictor.propagate_in_video(state):
                    prob = torch.sigmoid(logits[0, 0].float()).cpu().numpy()
                    out[run[k]] = np.clip(prob * 255.0 + 0.5, 0, 255).astype(np.uint8)
                    done += 1
                    progress(0.06 + 0.93 * done / total)
                    emit(type="at", frame=run[k])
                predictor.reset_state(state)
            carried = np.asarray(out[run[-1]]) > 127
            del state
    shutil.rmtree(folder, ignore_errors=True)
    out.flush()
    progress(1.0)
    emit(type="matte", path=args.out, frames=count, width=w, height=h)


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    c = sub.add_parser("check")
    c.add_argument("--src", required=True)
    s = sub.add_parser("segment")
    for name in ["--src", "--checkpoint", "--config", "--frames", "--points", "--out", "--work"]:
        s.add_argument(name, required=True)
    s.add_argument("--width", type=int, required=True)
    s.add_argument("--height", type=int, required=True)
    s.add_argument("--frame", type=int, required=True)
    args = parser.parse_args()
    try:
        {"check": check, "segment": segment}[args.command](args)
    except Exception as e:  # reported to the editor, which shows it
        emit(type="error", message=f"{type(e).__name__}: {e}")
        sys.exit(1)


if __name__ == "__main__":
    main()
