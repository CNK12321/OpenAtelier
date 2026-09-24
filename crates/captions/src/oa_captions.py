"""OpenAtelier's bridge to faster-whisper.

Written into the caption engine's folder and run with the engine's own Python (see
`engine.rs`); it isn't installed anywhere else. It talks to the editor in JSON lines on
stdout — one object per line, each with a "type" — and leaves stderr to the libraries'
progress output, which the editor shows as it comes.

    python oa_captions.py download   --model small --models <dir>
    python oa_captions.py transcribe --model small --models <dir> --audio a.wav [--language en]
"""

import argparse
import json
import os
import sys


def emit(**event):
    print(json.dumps(event), flush=True)


def model_dir(models, name):
    return os.path.join(models, name)


def download(args):
    from faster_whisper import download_model

    path = download_model(args.model, output_dir=model_dir(args.models, args.model))
    emit(type="done", path=path)


def transcribe(args):
    from faster_whisper import WhisperModel

    where = model_dir(args.models, args.model)
    if not os.path.isdir(where):
        where = args.model  # not downloaded into our folder: let faster-whisper fetch it
    # The CPU with 8-bit weights works everywhere; a GPU needs NVIDIA's CUDA libraries,
    # which the engine doesn't install.
    model = WhisperModel(where, device="cpu", compute_type="int8", cpu_threads=os.cpu_count() or 4)
    segments, info = model.transcribe(
        args.audio,
        language=args.language or None,
        word_timestamps=True,
        vad_filter=True,
        condition_on_previous_text=False,
    )
    emit(type="info", language=info.language, duration=info.duration)
    for seg in segments:
        words = [
            {"start": w.start, "end": w.end, "word": w.word, "p": w.probability}
            for w in (seg.words or [])
        ]
        emit(type="segment", start=seg.start, end=seg.end, text=seg.text, words=words)
    emit(type="done", path="")


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("download", "transcribe"):
        p = sub.add_parser(name)
        p.add_argument("--model", required=True)
        p.add_argument("--models", required=True)
        if name == "transcribe":
            p.add_argument("--audio", required=True)
            p.add_argument("--language", default="")
    args = parser.parse_args()
    try:
        download(args) if args.command == "download" else transcribe(args)
    except Exception as e:  # reported to the editor rather than as a traceback
        emit(type="error", message=f"{type(e).__name__}: {e}")
        sys.exit(1)


if __name__ == "__main__":
    main()
