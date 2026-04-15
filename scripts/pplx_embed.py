#!/usr/bin/env python3
"""Local embedding helper for the Perplexity 0.6B embedding family.

This is intentionally a client-side/dev utility. The backend does not call it.
It prefers devices in this order when `--device auto` is used:
1. cuda
2. mps
3. cpu
"""

from __future__ import annotations

import argparse
import functools
import json
import sys
from typing import Iterable, Sequence


DEFAULT_MODEL_ID = "perplexity-ai/pplx-embed-v1-0.6b"


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Generate local embeddings with PyTorch.")
    parser.add_argument("--model-id", default=DEFAULT_MODEL_ID)
    parser.add_argument(
        "--device",
        default="auto",
        choices=["auto", "cuda", "mps", "cpu"],
        help="Device preference. auto selects cuda -> mps -> cpu.",
    )
    parser.add_argument("--batch-size", type=int, default=8)
    parser.add_argument("--max-length", type=int, default=32768)
    parser.add_argument(
        "--text",
        action="append",
        default=[],
        help="Text to embed. May be specified multiple times.",
    )
    parser.add_argument(
        "--json-input",
        help="Path to a JSON file containing either a list of strings or {\"texts\": [...]}",
    )
    parser.add_argument(
        "--normalize",
        action="store_true",
        help="L2-normalize the output embeddings before printing them.",
    )
    return parser.parse_args(argv)


def select_device(requested: str = "auto", torch_module=None) -> str:
    if requested != "auto":
        return requested

    torch_module = torch_module or _import_torch()
    if hasattr(torch_module, "cuda") and torch_module.cuda.is_available():
        return "cuda"

    backends = getattr(torch_module, "backends", None)
    mps = getattr(backends, "mps", None)
    if mps is not None and mps.is_available():
        return "mps"

    return "cpu"


def load_texts(args: argparse.Namespace) -> list[str]:
    texts = [text for text in args.text if text.strip()]
    if args.json_input:
        with open(args.json_input, "r", encoding="utf-8") as handle:
            payload = json.load(handle)
        if isinstance(payload, dict):
            payload = payload.get("texts", [])
        if not isinstance(payload, list) or not all(isinstance(item, str) for item in payload):
            raise ValueError("json input must be a list of strings or {\"texts\": [...]}")  # noqa: TRY003
        texts.extend(item for item in payload if item.strip())
    if not texts:
        raise ValueError("at least one --text or --json-input entry is required")  # noqa: TRY003
    return texts


def embed_texts(
    texts: Sequence[str],
    model_id: str,
    requested_device: str = "auto",
    batch_size: int = 8,
    max_length: int = 32768,
    normalize: bool = False,
) -> dict:
    torch = _import_torch()
    transformers = _import_transformers()
    device = select_device(requested_device, torch)
    tokenizer, model = load_model_bundle(model_id, device)

    embeddings = []
    for batch in batched(texts, batch_size):
        encoded = tokenizer(
            list(batch),
            padding=True,
            truncation=True,
            max_length=max_length,
            return_tensors="pt",
        )
        encoded = {key: value.to(device) for key, value in encoded.items()}

        with torch.no_grad():
            outputs = model(**encoded)
            if hasattr(outputs, "sentence_embeddings"):
                batch_embeddings = outputs.sentence_embeddings
            else:
                batch_embeddings = mean_pool(
                    outputs.last_hidden_state,
                    encoded["attention_mask"],
                    torch,
                )
            if normalize:
                batch_embeddings = torch.nn.functional.normalize(batch_embeddings, p=2, dim=1)
            embeddings.extend(batch_embeddings.cpu().tolist())

    return {
        "model_id": model_id,
        "device": device,
        "count": len(texts),
        "dims": len(embeddings[0]) if embeddings else 0,
        "embeddings": embeddings,
}


@functools.lru_cache(maxsize=4)
def load_model_bundle(model_id: str, device: str):
    torch_module = _import_torch()
    transformers_module = _import_transformers()
    model_kwargs = {
        "trust_remote_code": True,
        "low_cpu_mem_usage": True,
    }
    if device == "cuda" and hasattr(torch_module, "float16"):
        model_kwargs["torch_dtype"] = torch_module.float16
    model = transformers_module.AutoModel.from_pretrained(model_id, **model_kwargs)
    model.to(device)
    model.eval()
    tokenizer = transformers_module.AutoTokenizer.from_pretrained(
        model_id,
        trust_remote_code=True,
    )
    return tokenizer, model


def mean_pool(last_hidden_state, attention_mask, torch_module):
    mask = attention_mask.unsqueeze(-1).expand(last_hidden_state.size()).float()
    summed = (last_hidden_state * mask).sum(dim=1)
    counts = mask.sum(dim=1).clamp(min=1e-9)
    return summed / counts


def batched(items: Sequence[str], size: int) -> Iterable[Sequence[str]]:
    for index in range(0, len(items), size):
        yield items[index : index + size]


def _import_torch():
    try:
        import torch  # type: ignore
    except ImportError as exc:  # pragma: no cover - exercised at runtime, not in unit tests
        raise RuntimeError("torch is required to run the local embedding helper") from exc
    return torch


def _import_transformers():
    try:
        import transformers  # type: ignore
    except ImportError as exc:  # pragma: no cover - exercised at runtime, not in unit tests
        raise RuntimeError("transformers is required to run the local embedding helper") from exc
    return transformers


def main(argv: Sequence[str] | None = None) -> int:
    try:
        args = parse_args(argv)
        texts = load_texts(args)
        result = embed_texts(
            texts=texts,
            model_id=args.model_id,
            requested_device=args.device,
            batch_size=args.batch_size,
            max_length=args.max_length,
            normalize=args.normalize,
        )
        json.dump(result, sys.stdout)
        sys.stdout.write("\n")
        return 0
    except Exception as exc:  # pragma: no cover - CLI surface
        print(f"error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
