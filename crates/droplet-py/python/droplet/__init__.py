"""Droplet Python SDK.

M0 just re-exports the native module. The real SDK surface (Catalog, Session,
backend + connector config) grows here in later milestones (PRODUCT.md §17).
"""

from ._droplet import add

__all__ = ["add"]
