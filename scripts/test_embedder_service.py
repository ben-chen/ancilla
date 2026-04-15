import unittest

from fastapi.testclient import TestClient

from embedder_service import EmbedderSettings, build_app, load_settings


class EmbedderServiceTests(unittest.TestCase):
    def test_load_settings_reads_environment(self):
        settings = load_settings(
            {
                "ANCILLA_EMBEDDER_HOST": "127.0.0.1",
                "ANCILLA_EMBEDDER_PORT": "4100",
                "ANCILLA_EMBEDDER_DEVICE": "cuda",
                "ANCILLA_EMBEDDER_BATCH_SIZE": "16",
                "ANCILLA_EMBEDDER_MAX_LENGTH": "8192",
                "ANCILLA_EMBEDDER_DEFAULT_MODEL_ID": "test-model",
                "ANCILLA_EMBEDDER_NORMALIZE": "true",
            }
        )
        self.assertEqual(settings.host, "127.0.0.1")
        self.assertEqual(settings.port, 4100)
        self.assertEqual(settings.device, "cuda")
        self.assertEqual(settings.batch_size, 16)
        self.assertEqual(settings.max_length, 8192)
        self.assertEqual(settings.default_model_id, "test-model")
        self.assertTrue(settings.normalize)

    def test_healthz_reports_default_model(self):
        app = build_app(EmbedderSettings(default_model_id="test-model"))
        client = TestClient(app)

        response = client.get("/healthz")
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()["default_model_id"], "test-model")

    def test_embed_endpoint_uses_request_model_and_returns_vectors(self):
        calls = []

        def fake_embed_fn(**kwargs):
            calls.append(kwargs)
            return {
                "model_id": kwargs["model_id"],
                "device": kwargs["requested_device"],
                "count": len(kwargs["texts"]),
                "dims": 3,
                "embeddings": [[1.0, 0.0, 0.0] for _ in kwargs["texts"]],
            }

        app = build_app(
            EmbedderSettings(device="cpu", default_model_id="default-model"),
            embed_fn=fake_embed_fn,
        )
        client = TestClient(app)

        response = client.post(
            "/v1/embed",
            json={
                "model_id": "custom-model",
                "texts": ["hello", "world"],
                "normalize": True,
            },
        )
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()["model_id"], "custom-model")
        self.assertEqual(len(response.json()["embeddings"]), 2)
        self.assertEqual(
            calls,
            [
                {
                    "texts": ["hello", "world"],
                    "model_id": "custom-model",
                    "requested_device": "cpu",
                    "batch_size": 2,
                    "max_length": 8192,
                    "normalize": True,
                }
            ],
        )

    def test_embed_endpoint_rejects_blank_payloads(self):
        app = build_app()
        client = TestClient(app)

        response = client.post("/v1/embed", json={"texts": ["   ", ""]})
        self.assertEqual(response.status_code, 400)
        self.assertIn("non-empty", response.text)


if __name__ == "__main__":
    unittest.main()
