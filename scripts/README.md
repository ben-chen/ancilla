# scripts

Local Ancilla helper utilities live in this `uv` project.
Use `uv` here instead of installing packages globally with `pip`.

## Setup

```bash
cd scripts
uv sync
```

## Embed helper

```bash
cd scripts
uv run python pplx_embed.py \
  --model-id perplexity-ai/pplx-embed-v1-0.6b \
  --device auto \
  --text "I prefer Rust for backend services."
```

The first run may download model code and weights from Hugging Face before it prints results.

## Tests

```bash
cd scripts
uv run python -m unittest test_pplx_embed.py
```
