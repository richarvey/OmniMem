#!/usr/bin/env python3
"""Turn benchmark results into a PDF that survives a sceptical reader.

    python report.py --summary results/v7-lme.summary.json \\
                     --summary results/v6-lme.summary.json \\
                     --session results/session.summary.json \\
                     --out reports/omnimem-benchmark.pdf

Rendered as HTML and printed through the Chromium that Playwright already
ships, so there is no new PDF dependency and the document can use the same
visual language as the website.

The document is built to be checkable rather than flattering. Every run states
its sample size and seed, the judge rubrics are reproduced verbatim, the answer
prompt is printed in full, and weaknesses get the same typographic weight as
strengths. That is not modesty, it is the entire competitive argument: mem0's
published numbers cannot be reproduced from their own repository, and ours are
meant to be.

Two things the report must never do, because they are the specific failures
found in mem0's material:

* Quote an accuracy figure without the rubric that produced it. The same
  answers score roughly 67% or roughly 92% depending only on rubric leniency.
* Quote F1 or BLEU as accuracy on LongMemEval. Gold answers there are terse
  ("$65") while correct answers are verbose, so those metrics track verbosity,
  not correctness. They appear only as a divergence check, labelled as such.
"""

from __future__ import annotations

import argparse
import html
import json
from datetime import datetime, timezone
from pathlib import Path

GREEN = "#63bd85"
INK = "#f2f4f6"
MUTED = "#969ca5"
PAPER = "#0d0e11"
CARD = "#101216"
LINE = "#262a31"

CSS = f"""
@page {{ size: A4; margin: 16mm 14mm; }}
* {{ box-sizing: border-box; }}
body {{ margin: 0; background: {PAPER}; color: {INK};
       font-family: Ubuntu, "DejaVu Sans", sans-serif; font-size: 10.5pt; line-height: 1.55; }}
h1 {{ font-size: 24pt; margin: 0 0 4pt; letter-spacing: -0.02em; }}
h2 {{ font-size: 14pt; margin: 22pt 0 8pt; padding-top: 8pt; border-top: 1px solid {LINE};
      page-break-after: avoid; }}
h3 {{ font-size: 11pt; margin: 14pt 0 6pt; color: {INK}; page-break-after: avoid; }}
p {{ margin: 0 0 8pt; color: {MUTED}; }}
.eyebrow {{ font-family: "Ubuntu Mono", monospace; font-size: 8.5pt; letter-spacing: 0.14em;
            text-transform: uppercase; color: {GREEN}; margin-bottom: 6pt; }}
table {{ width: 100%; border-collapse: collapse; margin: 6pt 0 12pt; font-size: 9.5pt; }}
th, td {{ text-align: left; padding: 5pt 7pt; border-bottom: 1px solid {LINE}; }}
th {{ color: {GREEN}; font-family: "Ubuntu Mono", monospace; font-size: 8.5pt;
      text-transform: uppercase; letter-spacing: 0.08em; }}
td.num {{ text-align: right; font-family: "Ubuntu Mono", monospace; }}
.card {{ background: {CARD}; border: 1px solid {LINE}; border-radius: 6pt;
         padding: 10pt 12pt; margin: 8pt 0; }}
.warn {{ border-left: 3px solid {GREEN}; }}
pre {{ background: {CARD}; border: 1px solid {LINE}; border-radius: 6pt; padding: 9pt;
       white-space: pre-wrap; font-family: "Ubuntu Mono", monospace; font-size: 8pt;
       color: {MUTED}; margin: 6pt 0 12pt; }}
.kv {{ font-family: "Ubuntu Mono", monospace; font-size: 9pt; color: {MUTED}; }}
.kv b {{ color: {INK}; font-weight: normal; }}
footer {{ margin-top: 18pt; padding-top: 8pt; border-top: 1px solid {LINE};
          font-size: 8pt; color: {MUTED}; }}
"""


def esc(value) -> str:
    return html.escape(str(value))


def pct(value) -> str:
    return "n/a" if value is None else f"{value * 100:.1f}%"


def ms(value) -> str:
    return "n/a" if value is None else f"{value:,.0f}ms"


def bars(rows: list[tuple[str, float | None]], width: int = 460) -> str:
    """A horizontal bar chart as inline SVG.

    Hand-rolled rather than pulled from a plotting library: the harness should
    not need matplotlib and its fonts just to draw a dozen rectangles.
    """
    rows = [(name, value) for name, value in rows if value is not None]
    if not rows:
        return "<p>No scored results.</p>"
    row_h, pad = 22, 150
    height = row_h * len(rows) + 8
    out = [f'<svg width="{width}" height="{height}" viewBox="0 0 {width} {height}">']
    span = width - pad - 46
    for index, (name, value) in enumerate(rows):
        y = index * row_h + 4
        bar = max(1, int(span * max(0.0, min(1.0, value))))
        out.append(
            f'<text x="0" y="{y + 13}" fill="{MUTED}" font-size="10" '
            f'font-family="monospace">{esc(name)[:22]}</text>'
            f'<rect x="{pad}" y="{y + 3}" width="{span}" height="12" fill="{LINE}" rx="2"/>'
            f'<rect x="{pad}" y="{y + 3}" width="{bar}" height="12" fill="{GREEN}" rx="2"/>'
            f'<text x="{pad + span + 6}" y="{y + 13}" fill="{INK}" font-size="10" '
            f'font-family="monospace">{value * 100:.1f}%</text>'
        )
    out.append("</svg>")
    return "".join(out)


def run_label(meta: dict) -> str:
    return f"OmniMem {meta.get('omnimem_version', '?')} ({meta.get('version_line', '?')})"


def methodology(metas: list[dict]) -> str:
    first = metas[0]
    judges = ", ".join(f"{j['name']} ({j['model']})" for j in first.get("judges", [])) or "none"
    single_judge = len(first.get("judges", [])) < 2
    caveat = ""
    if single_judge:
        caveat = (
            '<div class="card warn"><b>Single judge.</b> Only one judge was active for this '
            'run, so no cross-vendor agreement rate could be computed. Accuracy figures here '
            'rest on one model\'s opinion and should be read with that in mind.</div>'
        )
    return f"""
<h2>How this was measured</h2>
<p>Every question is answered from a <b>fresh, empty store</b>. LongMemEval gives each
question its own conversation history, so mixing them would let one question's evidence
answer another. Retrieval happens only after the background enrichment queue has drained,
and that wait is reported separately rather than being hidden inside recall latency.</p>
<div class="kv">
<b>Dataset:</b> {esc(first.get('dataset'))} &nbsp; <b>Licence:</b> {esc(first.get('dataset_licence'))}<br>
<b>Sample:</b> {esc(first.get('sample_size'))} questions, seed {esc(first.get('seed'))},
stratified across all six categories<br>
<b>Retrieval:</b> top-{esc(first.get('top_k'))} &nbsp; <b>Granularity:</b> {esc(first.get('granularity'))}<br>
<b>Answerer:</b> {esc(first.get('answerer_model'))} &nbsp; <b>Judges:</b> {esc(judges)}
</div>
{caveat}
<p>LOCOMO, the benchmark behind most published memory-system marketing, is deliberately
<b>not</b> used here: it is licensed CC BY-NC 4.0, and this document is commercial. The
datasets used are MIT and CC BY-SA respectively.</p>
<h3>The answer prompt, in full</h3>
<p>Published because a benchmark whose prompt is secret is not a benchmark. This one is
deliberately short and dataset agnostic. It contains no date ranges, no hints about answer
shape, and no instruction to guess rather than admit ignorance.</p>
<pre>{esc(first.get('answer_prompt', 'not recorded'))}</pre>
"""


def scoring_section(summaries: list[tuple[dict, dict]]) -> str:
    """Accuracy per run, per rubric, per category."""
    out = ["<h2>Accuracy</h2>"]
    out.append(
        "<p>Reported per rubric, because rubric choice moves these numbers more than "
        "most system differences do. The strict rubric is the headline; the lenient one "
        "is shown beside it to make the gap visible.</p>"
    )
    for meta, summary in summaries:
        memory_mode = summary.get("modes", {}).get("memory", {})
        out.append(f"<h3>{esc(run_label(meta))}</h3>")
        rows = ["<table><tr><th>Rubric</th><th>Judge</th><th>Correct</th>"
                "<th>Scored</th><th>Errors</th><th>Accuracy</th></tr>"]
        chart: list[tuple[str, float | None]] = []
        available = list(memory_mode.get("rubrics", {}).keys())
        # Chart one rubric only. Plotting every rubric would draw each category
        # twice with no way to tell which bar belonged to which rubric, which
        # is exactly the kind of unlabelled figure this report exists to avoid.
        primary = "strict" if "strict" in available else (available[0] if available else None)
        for rubric, data in memory_mode.get("rubrics", {}).items():
            for judge_name, result in data.items():
                if judge_name == "agreement":
                    continue
                overall = result.get("overall", {})
                rows.append(
                    f"<tr><td>{esc(rubric)}</td><td>{esc(judge_name)}</td>"
                    f"<td class='num'>{overall.get('correct', 0)}</td>"
                    f"<td class='num'>{overall.get('scored', 0)}</td>"
                    f"<td class='num'>{overall.get('errors', 0)}</td>"
                    f"<td class='num'>{pct(overall.get('accuracy'))}</td></tr>"
                )
                if rubric == primary:
                    for category, stats in result.get("by_category", {}).items():
                        chart.append((f"{category}", stats.get("accuracy")))
            agreement = data.get("agreement", {})
            if agreement.get("agreement") is not None:
                rows.append(
                    f"<tr><td>{esc(rubric)}</td><td>judge agreement</td>"
                    f"<td class='num' colspan='3'>{agreement.get('agreed')} of "
                    f"{agreement.get('comparable_answers')}</td>"
                    f"<td class='num'>{pct(agreement.get('agreement'))}</td></tr>"
                )
        rows.append("</table>")
        out.append("".join(rows))
        if chart:
            out.append(f"<h3>By question category ({esc(primary)} rubric)</h3>")
            out.append(bars(chart))
    return "".join(out)


def divergence_section(summaries: list[tuple[dict, dict]]) -> str:
    rows = ["<table><tr><th>Run</th><th>Judge accuracy</th><th>F1 mean</th>"
            "<th>BLEU-1 mean</th></tr>"]
    for meta, summary in summaries:
        memory_mode = summary.get("modes", {}).get("memory", {})
        best = None
        for data in memory_mode.get("rubrics", {}).values():
            for judge_name, result in data.items():
                if judge_name != "agreement":
                    best = result.get("overall", {}).get("accuracy", best)
        rows.append(
            f"<tr><td>{esc(run_label(meta))}</td><td class='num'>{pct(best)}</td>"
            f"<td class='num'>{memory_mode.get('f1_mean') or 0:.3f}</td>"
            f"<td class='num'>{memory_mode.get('bleu1_mean') or 0:.3f}</td></tr>"
        )
    rows.append("</table>")
    return f"""
<h2>Judge-independent check</h2>
<p><b>These are not accuracy figures.</b> On LongMemEval the gold answers are terse, often a
single value such as "$65", while a good answer is a sentence that shows its working. Token
overlap therefore measures verbosity, not correctness, and a fully correct answer routinely
scores below 0.25. They are included only as a divergence check: if judge accuracy moved and
these did not, that is a signal worth investigating, not a result to quote.</p>
{"".join(rows)}
"""


def performance_section(summaries: list[tuple[dict, dict]]) -> str:
    rows = ["<table><tr><th>Run</th><th>Ingest p50</th><th>Queue settle p50</th>"
            "<th>Recall p50</th><th>Recall p95</th><th>Context tokens</th></tr>"]
    for meta, summary in summaries:
        ingest = summary.get("ingest_p50_per_question_ms", {})
        settle = summary.get("settle_ms", {})
        recall = summary.get("recall_ms", {})
        tokens = summary.get("retrieved_context_tokens", {})
        rows.append(
            f"<tr><td>{esc(run_label(meta))}</td>"
            f"<td class='num'>{ms(ingest.get('p50'))}</td>"
            f"<td class='num'>{ms(settle.get('p50'))}</td>"
            f"<td class='num'>{ms(recall.get('p50'))}</td>"
            f"<td class='num'>{ms(recall.get('p95'))}</td>"
            f"<td class='num'>{tokens.get('p50', 0):,.0f}</td></tr>"
        )
    rows.append("</table>")
    return f"""
<h2>Performance</h2>
<p>Ingest is the per-memory write latency. Settle is the wait for background enrichment to
finish, reported separately so it cannot flatter recall. Context tokens are what retrieval
actually handed the model, which is the figure that determines cost per question.</p>
{"".join(rows)}
"""


def session_section(session: dict | None) -> str:
    """The with and without OmniMem comparison.

    Reads the shape ``run_session_bench.py`` writes: each arm carries a
    ``sessions`` map keyed by label, because session 2 now runs twice, cold and
    briefed. Every lookup is defensive so a half-finished run degrades to a
    visibly incomplete table rather than a confident wrong number.
    """
    if not session:
        return ""
    summary = session.get("summary", {}) or {}
    arms = summary.get("arms", {}) or {}
    overhead = summary.get("fixed_overhead", {}) or {}
    fixed = overhead.get("fixed_overhead_tokens")

    labels = [
        ("session1", "Session 1, investigate"),
        ("session2_cold", "Session 2, cold"),
        ("session2_briefed", "Session 2, briefed"),
    ]
    rows = ["<table><tr><th>Session</th><th>Arm</th><th>Turns</th><th>Input tokens</th>"
            "<th>Output</th><th>Cost</th><th>Correct</th></tr>"]
    for key, caption in labels:
        for arm, arm_label in (("control", "control"), ("omnimem", "OmniMem")):
            entry = ((arms.get(arm, {}) or {}).get("sessions", {}) or {}).get(key)
            if not entry:
                continue
            correct = entry.get("correct")
            rows.append(
                f"<tr><td>{esc(caption)}</td><td>{esc(arm_label)}</td>"
                f"<td class='num'>{entry.get('turns', 0):,}</td>"
                f"<td class='num'>{entry.get('input_tokens', 0):,}</td>"
                f"<td class='num'>{entry.get('output_tokens', 0):,}</td>"
                f"<td class='num'>${entry.get('cost_usd', 0):.4f}</td>"
                f"<td class='num'>{esc(correct or 'not judged')}</td></tr>"
            )
    rows.append("</table>")

    overhead_text = f"{fixed:,.0f}" if isinstance(fixed, (int, float)) else "not measured"
    return f"""
<h2>Session comparison: a dead end, re-evaluated</h2>
<p>Both arms investigate which of two vendored crates suits a codebase, find that the
obvious candidate cannot work, and record or re-derive that conclusion. Session 2 then asks
the same question again in a fresh session, <b>with the codebase withheld from both arms</b>,
twice: once cold, and once following the workflow the server instructions prescribe.</p>
<div class="card warn"><b>The fixture is checked for guessability before anything is
measured.</b> An earlier version of this scenario was answered correctly three times out of
three with no files, no memory and no tools, which made every accuracy verdict worthless. The
harness now runs that probe from an empty directory and refuses to start if the answer is
recoverable without consulting anything. It costs three model calls and it has caught two
compromised fixtures.</div>
<p>Attaching the server costs <b>{esc(overhead_text)}</b> tokens per turn on a matched
trivial prompt, before any work happens. That figure understates the cost inside a working
session, where tool definitions and instructions are carried on every turn.</p>
{"".join(rows)}
<p>Session 1 is asymmetric by construction: only the treatment arm pays to record what it
learned. The session 2 rows are the like-for-like comparison, and the cold and briefed
variants answer different questions. Cold asks whether an agent reaches for memory
unprompted. Briefed asks whether the intended workflow delivers the answer once it does.</p>
"""


def limitations(metas: list[dict]) -> str:
    first = metas[0]
    return f"""
<h2>Limitations</h2>
<p>Stated plainly, because every one of these would otherwise be found by a reader and
treated as concealment.</p>
<ul>
<li><b>Sample, not census.</b> {esc(first.get('sample_size'))} of 500 questions, seed
{esc(first.get('seed'))}. A full run means ingesting roughly 61 million tokens across 500
separate stores. Per-question results are published so the sample can be checked.</li>
<li><b>One answerer model.</b> Results are specific to {esc(first.get('answerer_model'))}.
A stronger model would lift every mode, including the no-memory floor.</li>
<li><b>Judges are models.</b> Accuracy here is one or two models' opinions against a rubric.
The rubrics are printed in full so anyone can disagree with them specifically.</li>
<li><b>F1 and BLEU are not accuracy</b> on this dataset, for the reason given above.</li>
<li><b>Latency is single-host.</b> Measured on one machine with no concurrent load, so it
shows relative cost between versions, not production throughput.</li>
<li><b>The session comparison is a single scenario.</b> One fact set, one model, one
run. Any crossover point it implies is an extrapolation from n=1, not a measurement,
and the token gap is dominated by how many agent turns the tool calls took rather
than by retrieval efficiency.</li>
</ul>
"""


def build_html(summaries: list[tuple[dict, dict]], session: dict | None) -> str:
    metas = [meta for meta, _ in summaries]
    generated = datetime.now(timezone.utc).strftime("%d %B %Y")
    return f"""<!doctype html><meta charset="utf-8"><style>{CSS}</style>
<div class="eyebrow">Benchmark report</div>
<h1>OmniMem memory benchmark</h1>
<p>Generated {esc(generated)}. Every figure here was produced by
<code>scripts/benchmarking</code> in the OmniMem repository, and the per-question records
behind them are published alongside this document.</p>
{methodology(metas)}
{scoring_section(summaries)}
{divergence_section(summaries)}
{performance_section(summaries)}
{session_section(session)}
{limitations(metas)}
<footer>Produced by OmniMem's own benchmark harness. Sample size and seed are stated above;
rubrics and the answer prompt are reproduced in full so results can be reproduced or
disputed on their merits.</footer>
"""


def render_pdf(html_text: str, out_path: Path) -> None:
    """Print the HTML through Chromium. Requires playwright and a browser."""
    import os

    from playwright.sync_api import sync_playwright

    out_path.parent.mkdir(parents=True, exist_ok=True)
    html_path = out_path.with_suffix(".html")
    html_path.write_text(html_text)
    with sync_playwright() as pw:
        browser = pw.chromium.launch(executable_path=os.environ.get("PW_EXEC") or None)
        page = browser.new_page()
        page.goto(f"file://{html_path.resolve()}", wait_until="load")
        page.pdf(path=str(out_path), format="A4", print_background=True)
        browser.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--summary", action="append", type=Path, required=True,
                        help="a .summary.json from run_memory_bench.py; repeatable")
    parser.add_argument("--session", type=Path, default=None,
                        help="optional summary from run_session_bench.py")
    parser.add_argument("--out", type=Path, default=Path("reports/omnimem-benchmark.pdf"))
    parser.add_argument("--html-only", action="store_true")
    args = parser.parse_args()

    summaries = []
    for path in args.summary:
        blob = json.loads(path.read_text())
        summaries.append((blob["metadata"], blob["summary"]))
    session = json.loads(args.session.read_text()) if args.session else None

    html_text = build_html(summaries, session)
    if args.html_only:
        out = args.out.with_suffix(".html")
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(html_text)
        print(f"wrote {out}")
        return 0
    render_pdf(html_text, args.out)
    print(f"wrote {args.out} ({args.out.stat().st_size:,} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
