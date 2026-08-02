def transpose(rows):
    """Transpose a rectangular matrix. A ragged one raises ValueError."""
    if not rows:
        return []
    width = len(rows[0])
    if any(len(row) != width for row in rows):
        raise ValueError("ragged matrix")
    return [[row[i] for row in rows] for i in range(width)]
