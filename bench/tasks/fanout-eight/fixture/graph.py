"""Unweighted shortest paths via BFS on an adjacency-list graph."""

from collections import deque


def shortest_path(adj, src, dst):
    """adj: dict node -> list of neighbours. Returns the shortest path as a
    list of nodes from src to dst inclusive, or None if unreachable."""
    if src == dst:
        return [src]
    visited = {src}
    queue = deque([(src, [src])])
    while queue:
        node, path = queue.pop()
        for nb in adj.get(node, []):
            if nb in visited:
                continue
            if nb == dst:
                return path + [nb]
            visited.add(nb)
            queue.append((nb, path + [nb]))
    return None
