# SQL Schema And Retrieval Design

Ancilla now treats a memory as one canonical markdown document plus machine metadata.

## Memory Shape

The user-facing source of truth is `memory_records.content_markdown`.

Canonical format:

```md
# Building Ancilla

Tags: project, ancilla

I am building Ancilla, a personal memory system.
```

The markdown document carries the human-facing content:

- title in the H1 line
- optional tags in the `Tags:` line
- the rest of the memory body as normal markdown

Postgres still stores additional machine metadata separately:

- `kind`
- `attrs`
- temporal fields like `observed_at`, `valid_from`, `valid_to`
- `state`
- lineage/thread/path fields
- embedding vectors

## Derived Search Text

Ancilla no longer stores separate authored `display_text` and `retrieval_text` fields for memories.

Instead:

- `content_markdown` is canonical
- `search_text` is derived plain text

`search_text` is what lexical search and embeddings operate on. It is generated from:

- the parsed title
- parsed tags
- flattened markdown body

This keeps one editable memory representation while still giving the retrieval system a clean text projection.

## Creation Paths

There are now two distinct ways to create memories:

1. Explicit markdown store:
   `POST /v1/memories`

   The caller sends a canonical markdown document directly and Ancilla stores it as a memory.

2. Model-backed memory generation:
   `POST /v1/memories/generate`

   The caller sends freeform context text. Ancilla runs the runtime prompt in [`prompts/memory_creation.md`](../prompts/memory_creation.md), asks the configured chat model for structured memory candidates, then renders those into canonical markdown before storage.

The runtime prompt explicitly allows returning zero memories. That is intentional.

## Retrieval Flow

Current retrieval is application-driven rather than SQL-driven:

1. Build the query text from the latest user message and recent turns.
2. If available, derive a query embedding from the live embedder service.
3. Score candidate memories using:
   - lexical overlap against `search_text`
   - cosine similarity against `memory_embeddings`
4. Send the top shortlist to the gate model when configured.
5. Fall back to a small deterministic gate when the model gate is unavailable.
6. Persist retrieval traces for audit/debugging.

The stored SQL schema still matters for persistence and indexing, but the primary candidate ranking logic now lives in application code.

## Embedding Choice

The current default embed model is `perplexity-ai/pplx-embed-v1-0.6b`.

Important assumptions:

- embedding dimensionality is `1024`
- similarity is cosine
- the same base embedding space is used for stored memories and live query embeddings

If that model changes later, embeddings should be rebuilt consistently rather than mixed within the same index.
