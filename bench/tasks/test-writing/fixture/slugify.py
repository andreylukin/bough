import re


def slugify(text):
    """URL slug: lowercase; each run of non-alphanumeric characters becomes a
    single hyphen; no leading or trailing hyphens; input with no alphanumeric
    characters slugs to the empty string."""
    text = text.lower()
    text = re.sub(r"[^a-z0-9]+", "-", text)
    return text.strip("-")
