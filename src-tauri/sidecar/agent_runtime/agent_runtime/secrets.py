"""OS keychain lookup helpers for LLM provider API keys.

Charter §"Hard constraints" #2 forbids plaintext API keys: every
secret read by the agent runtime goes through this module which in
turn delegates to the platform's native keychain via the `keyring`
package (Windows Credential Manager, macOS Keychain, Secret Service /
KWallet on Linux).

The single service name is `neuron`; usernames are provider names
(`anthropic`, `openai`).

If a key is missing, `get_provider_key` raises `NoApiKey(provider)` so
the LangGraph node can convert that into a structured `attrs.error`
span without leaking the exception path through stdio.
"""

from __future__ import annotations

import os
from dataclasses import dataclass


# Service name used in the keychain for every Neuron secret. Pinned as
# a constant so a typo in a callsite can't accidentally read from a
# different service's namespace.
SERVICE = "neuron"


@dataclass
class NoApiKey(Exception):
    """Raised when a provider's API key is not configured.

    Surfaces to the UI as a span with `attrs.error='no_api_key'` and
    drives a "Configure API keys" CTA in the Settings route. The
    attached `provider` is the user-visible identifier
    (`'anthropic'`, `'openai'`).
    """

    provider: str

    def __str__(self) -> str:  # pragma: no cover - cosmetic
        return f"no API key configured for {self.provider}"


def get_provider_key(provider: str) -> str:
    """Return the API key for `provider`, or raise `NoApiKey`.

    Resolution order:

    1. Environment override `NEURON_<PROVIDER>_API_KEY` (uppercased).
       This is **only** for tests and developer escape hatches; never
       advertised in user docs.
    2. OS keychain via `keyring.get_password(SERVICE, provider)`.

    Lookup is case-insensitive on the provider name.
    """
    p = provider.lower()

    # Test-only env override. Used by `tests/test_daily_summary.py` so
    # the workflow under test does not have to touch the real OS
    # keychain. Production keys never go through env.
    env_name = f"NEURON_{p.upper()}_API_KEY"
    env_val = os.environ.get(env_name)
    if env_val:
        return env_val

    # `keyring` is imported lazily so that `import secrets` from a
    # context that does not need keychain access (e.g., the framing
    # tests) does not load the platform backend.
    import keyring

    val = keyring.get_password(SERVICE, p)
    if not val:
        raise NoApiKey(provider=p)
    return val


def has_provider_key(provider: str) -> bool:
    """Cheap presence check — returns False instead of raising."""
    try:
        get_provider_key(provider)
        return True
    except NoApiKey:
        return False
