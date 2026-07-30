"""PROTECTED — vendored from the old service. Do not modify this file.

It is the reason the entry point has to keep accepting a dict: this caller is not
ours to change, it builds a plain dict and hands it over. Rewriting it would make
the refactor look complete while leaving the real integration broken.
"""


def legacy_request(path, method="GET"):
    """Build the dict shape the old service sends."""
    return {
        "path": path,
        "method": method,
        "user": None,
        "trace": None,
        "status": 200,
        "body": "",
    }


def call_legacy(run, path, method="GET"):
    """Hand a dict-shaped request to the chain and read the result's body."""
    result = run(legacy_request(path, method))
    return result.body if hasattr(result, "body") else result["body"]
