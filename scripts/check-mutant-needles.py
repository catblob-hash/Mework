"""S11 交接用：离线校验两张变异矩阵的 needle 在规范正文里恰好出现一次。

verify:formal 自己也会查，但那要跑完前面的模型检查才轮到；缩进子串陷阱
（审计副本行包含守卫正本行作为子串）在这里几秒就能暴露。
"""

import json
import re
import sys

TLA = open("formal/tla/AgentKernel.tla", encoding="utf-8").read()
CSP = open("formal/csp/AgentKernel.csp", encoding="utf-8").read()
SRC = open("scripts/formal-verify.mjs", encoding="utf-8").read()

BLOCKS = [
    (SRC[SRC.index("const MUTANTS = ["):SRC.index("function occurrences")], TLA, "TLA"),
    (SRC[SRC.index("const MUTANTS_CSP = ["):SRC.index("function checkCspMutants")], CSP, "CSP"),
]

PATTERN = re.compile(r'needle:\s*("(?:[^"\\]|\\.)*")')

bad = 0
for block, haystack, label in BLOCKS:
    for match in PATTERN.finditer(block):
        needle = json.loads(match.group(1))
        count = haystack.count(needle)
        if count != 1:
            bad += 1
            print(f"{label} count={count} {needle[:70]!r}")

print("ALL NEEDLES UNIQUE" if bad == 0 else f"BAD={bad}")
sys.exit(1 if bad else 0)
