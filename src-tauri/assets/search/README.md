# Offline static search model

Source: [MinishLab potion-base-8M](https://huggingface.co/minishlab/potion-base-8M),
revision `bf8b056651a2c21b8d2565580b8569da283cab23`, MIT.
The upstream model has 29,528 WordPiece rows and 256 dimensions. Toolport stores
symmetric int8 rows with one float32 scale per row. The full MIT notice is appended
to the weights and therefore remains in every compiled gateway binary.

The loader uses the original tokenizer with native regex, HTTP and progress features
disabled. No training code runs during inference. Inference pools token vectors and normalizes the sum.
Catalog embeddings use the readable operation name and the first description
sentence, bounded to 512 characters. Queries and documents use at most 256 tokens.
No transformer runs. The gateway makes no network request for these vectors and
uses the same scalar CPU path on Linux, macOS and Windows.

Reproduce from a local download of that exact revision, on devbox with NumPy and
safetensors installed:

```bash
python3 scripts/prepare-search-model.py /path/to/potion-base-8M src-tauri/assets/search
sha256sum src-tauri/assets/search/model-q8.bin src-tauri/assets/search/tokenizer.json
```

Weighted reciprocal rank fusion uses `1 / (10 + lexical_rank)` plus
`0.5 / (10 + semantic_rank)`. Semantic contributions need cosine at least 0.35;
semantic-only candidates need 0.60 and at least 85% of the strongest cosine in
the visible pool. Confidence needs sufficient absolute lexical evidence and
separation from the runner-up in both retrieval paths. Provider identity is a
soft boost; explicit server filters and exact-name lookup remain scoped. Existing
user-configured endpoint search is retained; it is separate from this local path.
The model was not trained, pruned or distilled using Toolport labels or queries.

The explicit tests support benchmark-only model substitutions through
`SEARCH_STATIC_MODEL_DIR` (files `model.bin` and `tokenizer.json`) and lexical-only
measurements through `SEARCH_LEXICAL_ONLY`. Shipped binaries expose neither control.

Every gateway requires `search-static`, including headless builds and bundled
sidecars. Builds with `--no-default-features` must pass `--features search-static`;
Cargo rejects a gateway build without it. Model-off gateways are unsupported.
Production encoding uses one bounded
worker and a private content cache capped at 32 MiB and 14 days. Scoped searches
filter the main snapshot and cannot submit or replace worker jobs.
