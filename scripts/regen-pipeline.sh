#!/usr/bin/env bash
# KLL/合约再生成全管线：intake 是需求真源，KLL 与草稿均为派生物。
# 序列与 CLAUDE.md「修改需求后的再生成序列」一致，供逐合约修补后一键落地。
set -euo pipefail
cd "$(dirname "$0")/.."

agent-spec requirements import --from doc/requirements-intake.md --out knowledge/requirements

# import 会把 status 重置为 proposed；依据用户既定核准全部回到 accepted
for f in knowledge/requirements/req-fr-*.md; do
  id=$(awk '/^id: /{print $2; exit}' "$f")
  agent-spec requirements transition "$id" --to accepted >/dev/null
done
echo "accepted: $(rg -c '^status: accepted' knowledge/requirements | awk -F: '{s+=$2} END{print s}')"

rm -rf specs/generated
agent-spec requirements draft-specs --knowledge knowledge --out specs/generated
python3 scripts/refine-drafts.py
# 生成器会把需求 id 误写进草稿 depends:（该字段只接受 spec 名），删除悬空行
sed -i '/^depends: \[REQ-FR-/d' specs/generated/*.spec.md

agent-spec lint-knowledge --knowledge knowledge --gate
agent-spec requirements graph --knowledge knowledge --format json --gate
agent-spec requirements plan --knowledge knowledge --specs specs/generated --format json --gate

echo "PIPELINE OK — scenarios in intake: $(rg -c '^场景: ' doc/requirements-intake.md)"
