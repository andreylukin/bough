def toposort(edges, nodes):
    """Kahn's algorithm. Returns a topological order.

    `edges` is a list of (before, after) pairs. A cycle raises ValueError.
    Ties are broken by sorting, so the order is deterministic.
    """
    incoming = {n: 0 for n in nodes}
    out = {n: [] for n in nodes}
    for a, b in edges:
        out[a].append(b)
        incoming[b] += 1
    ready = sorted(n for n in nodes if incoming[n] == 0)
    order = []
    while ready:
        n = ready.pop(0)
        order.append(n)
        for m in out[n]:
            incoming[m] -= 1
            if incoming[m] == 0:
                ready.append(m)
        ready.sort()
    if len(order) != len(nodes):
        raise ValueError("cycle")
    return order
