"""Droplet Python SDK.

Re-exports the native module. The minimal binding surfaces the M1 local analyze engine
(``Engine`` over local Parquet, opaque ``Dataset`` handles, capped read-outs). The real SDK
surface (Catalog, Session, run_code, backend + connector config) grows here in later
milestones (PRODUCT.md §17).
"""

from ._droplet import Dataset, Engine

__all__ = ["Dataset", "Engine"]
