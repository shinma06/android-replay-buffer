"""Wire versions used by verification clients, from the server's negotiation table."""
import re
from pathlib import Path

_source = Path(__file__).resolve().parents[1] / "crates/arb-cli/src/mcp/version.rs"
VERSIONS = dict(re.findall(r'Self::(V[0-9_]+) => "([0-9-]+)"', _source.read_text(encoding="utf-8")))
