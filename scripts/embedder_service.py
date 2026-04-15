#!/usr/bin/env python3
"""HTTP embedding service for Ancilla."""

from __future__ import annotations

import os
from dataclasses import dataclass
from typing import Callable

import uvicorn
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel, Field

from pplx_embed import DEFAULT_MODEL_ID, embed_texts


@dataclass(frozen=True)
class EmbedderSettings:
    host: str = "0.0.0.0"
    port: int = 4000
    device: str = "auto"
    batch_size: int = 2
    max_length: int = 8192
    default_model_id: str = DEFAULT_MODEL_ID
    normalize: bool = False


class EmbedRequest(BaseModel):
    texts: list[str] = Field(min_length=1)
    model_id: str | None = None
    normalize: bool | None = None


class EmbedResponse(BaseModel):
    model_id: str
    device: str
    count: int
    dims: int
    embeddings: list[list[float]]


def load_settings(environ: dict[str, str] | None = None) -> EmbedderSettings:
    environ = environ or os.environ
    return EmbedderSettings(
        host=environ.get("ANCILLA_EMBEDDER_HOST", "0.0.0.0").strip() or "0.0.0.0",
        port=max(1, int(environ.get("ANCILLA_EMBEDDER_PORT", "4000"))),
        device=environ.get("ANCILLA_EMBEDDER_DEVICE", "auto").strip() or "auto",
        batch_size=max(1, int(environ.get("ANCILLA_EMBEDDER_BATCH_SIZE", "2"))),
        max_length=max(1, int(environ.get("ANCILLA_EMBEDDER_MAX_LENGTH", "8192"))),
        default_model_id=(
            environ.get("ANCILLA_EMBEDDER_DEFAULT_MODEL_ID", DEFAULT_MODEL_ID).strip()
            or DEFAULT_MODEL_ID
        ),
        normalize=environ.get("ANCILLA_EMBEDDER_NORMALIZE", "").strip().lower()
        in {"1", "true", "yes", "on"},
    )


def build_app(
    settings: EmbedderSettings | None = None,
    embed_fn: Callable[..., dict] = embed_texts,
) -> FastAPI:
    settings = settings or load_settings()
    app = FastAPI(title="Ancilla Embedder", version="0.1.0")

    @app.get("/healthz")
    async def healthz() -> dict[str, object]:
        return {
            "status": "ok",
            "default_model_id": settings.default_model_id,
            "device_preference": settings.device,
        }

    @app.post("/v1/embed", response_model=EmbedResponse)
    async def embed(request: EmbedRequest) -> EmbedResponse:
        texts = [text.strip() for text in request.texts if text.strip()]
        if not texts:
            raise HTTPException(status_code=400, detail="at least one non-empty text is required")

        model_id = (request.model_id or settings.default_model_id).strip()
        normalize = settings.normalize if request.normalize is None else request.normalize

        try:
            result = embed_fn(
                texts=texts,
                model_id=model_id,
                requested_device=settings.device,
                batch_size=settings.batch_size,
                max_length=settings.max_length,
                normalize=normalize,
            )
        except Exception as exc:  # pragma: no cover - runtime error path
            raise HTTPException(status_code=500, detail=str(exc)) from exc

        return EmbedResponse.model_validate(result)

    return app


def main() -> int:
    settings = load_settings()
    uvicorn.run(
        build_app(settings),
        host=settings.host,
        port=settings.port,
        log_level="info",
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
