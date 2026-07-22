"""Small dense-matrix helpers. Matrices are lists of equal-length rows."""


def transpose(m):
    if not m:
        return []
    rows, cols = len(m), len(m[0])
    return [[m[r][c] for r in range(rows)] for c in range(cols)]


def matmul(a, b):
    if len(a[0]) != len(b):
        raise ValueError("shape mismatch")
    n, k, p = len(a), len(b), len(b[0])
    out = [[0] * p for _ in range(n)]
    for i in range(n):
        for j in range(p):
            out[i][j] = sum(a[i][t] * b[j][t] for t in range(k))
    return out


def identity(n):
    return [[1 if i == j else 0 for j in range(n)] for i in range(n)]
