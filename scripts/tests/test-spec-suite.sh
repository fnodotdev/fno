#!/usr/bin/env bash
set -euo pipefail

bash tests/spec/test_blueprint_phase_close.sh
bash tests/spec/test_claims_arg.sh
bash tests/spec/test_executor_transcription.sh
# No project venv carries pytest any more; uv supplies it for these two
# stdlib-only spec files.
uv run --no-project --with pytest pytest -q \
  tests/spec/test_product_md_check.py \
  tests/spec/test_impeccable_stages_validator.py
