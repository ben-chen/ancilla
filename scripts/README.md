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

## Embedder service

The HTTP embedder service lives here too:

```bash
cd scripts
ANCILLA_EMBEDDER_DEVICE=cpu \
ANCILLA_EMBEDDER_PORT=4100 \
uv run python embedder_service.py
```

Then call it:

```bash
curl -X POST http://127.0.0.1:4100/v1/embed \
  -H 'content-type: application/json' \
  --data '{"texts":["I prefer Rust for backend services."]}'
```

In production, the embedder runs as a separate service and `ancilla-server` talks to it over `embedder_base_url`.

## Tests

```bash
cd scripts
uv run python -m unittest test_pplx_embed.py test_embedder_service.py
```
