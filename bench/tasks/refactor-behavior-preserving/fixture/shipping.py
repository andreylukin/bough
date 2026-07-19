"""Shipping desk: render a manifest line per record."""


def ship_manifest(lines):
    manifest = []
    for line in lines:
        if not line.strip():
            continue
        parts = line.split("|")
        if len(parts) != 3:
            raise ValueError(f"bad record: {line!r}")
        sku, qty, loc = parts
        qty = int(qty)
        manifest.append(f"{sku} x{qty} @ {loc}")
    return manifest
