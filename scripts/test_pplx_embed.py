import argparse
import tempfile
import unittest
from pathlib import Path

from scripts import pplx_embed


class _FakeAvailable:
    def __init__(self, available: bool):
        self._available = available

    def is_available(self):
        return self._available


class _FakeTorch:
    def __init__(self, cuda_available: bool, mps_available: bool):
        self.cuda = _FakeAvailable(cuda_available)

        class _Backends:
            def __init__(self, mps_available: bool):
                self.mps = _FakeAvailable(mps_available)

        self.backends = _Backends(mps_available)


class TestPplxEmbed(unittest.TestCase):
    def test_auto_device_prefers_cuda_then_mps_then_cpu(self):
        self.assertEqual(
            pplx_embed.select_device("auto", _FakeTorch(cuda_available=True, mps_available=True)),
            "cuda",
        )
        self.assertEqual(
            pplx_embed.select_device("auto", _FakeTorch(cuda_available=False, mps_available=True)),
            "mps",
        )
        self.assertEqual(
            pplx_embed.select_device("auto", _FakeTorch(cuda_available=False, mps_available=False)),
            "cpu",
        )

    def test_explicit_device_bypasses_detection(self):
        self.assertEqual(
            pplx_embed.select_device("cpu", _FakeTorch(cuda_available=True, mps_available=True)),
            "cpu",
        )

    def test_load_texts_from_args_and_json(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "input.json"
            path.write_text('{"texts": ["alpha", "beta"]}', encoding="utf-8")
            args = argparse.Namespace(text=["gamma"], json_input=str(path))
            self.assertEqual(pplx_embed.load_texts(args), ["gamma", "alpha", "beta"])


if __name__ == "__main__":
    unittest.main()
