"""Unit tests for logcat parsing."""

from __future__ import annotations

import unittest

from replay_buffer.log_buffer import _parse_logcat_line


class LogBufferTest(unittest.TestCase):
    def test_parse_threadtime_line(self) -> None:
        raw = "09-17 12:34:56.789  1234  5678 E AndroidRuntime: FATAL EXCEPTION: main"
        event = _parse_logcat_line(raw, host_time_ms=1000.0)
        self.assertIsNotNone(event)
        assert event is not None
        self.assertEqual(event.level, "E")
        self.assertEqual(event.tag, "AndroidRuntime")
        self.assertEqual(event.message, "FATAL EXCEPTION: main")
        self.assertEqual(event.source, "logcat")

    def test_parse_invalid_line_returns_none(self) -> None:
        self.assertIsNone(_parse_logcat_line("not a logcat line", 0.0))


if __name__ == "__main__":
    unittest.main()
