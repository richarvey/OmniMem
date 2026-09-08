"""The embedding benchmark harness, run against the in-memory fakes.

The real thing needs valkey-search and the real model; this keeps the
corpus builders, the measurement loop, the report shape and the comparison
maths from rotting between the before and after runs of a backend swap.
"""

import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent / "scripts"))

import embedding_bench as bench  # noqa: E402


@pytest.fixture(autouse=True)
def _reset_tools():
    import tools as tools_pkg
    yield
    tools_pkg._store = tools_pkg._embedder = tools_pkg._lifecycle = tools_pkg._pipeline = None


class TestCorpus:
    def test_synthetic_is_reproducible_and_namespaced(self):
        a = bench.synthetic_corpus(50, seed=3)
        b = bench.synthetic_corpus(50, seed=3)
        assert a == b
        assert {item["namespace"] for item in a} <= {"episodic", "knowledge", "preference"}
        assert all("bench" in item["tags"] for item in a)
        assert bench.synthetic_corpus(50, seed=4) != a

    def test_from_dump_keeps_text_only(self, tmp_path):
        dump = {"version": "6.6.2", "data": {
            "mem:episodic:01A": {"content": "first", "tags": json.dumps(["x", "y"]),
                                 "licence": "own", "vector": "binary"},
            "mem:knowledge:art": {"content": "article", "tags": "not json"},
            "mem:project:ctx": {"content": "ignored: project namespace"},
            "mem:skill:gen:x": {"content": "ignored: skill"},
            "mem:episodic:01B": {"content": "   "},
        }}
        path = tmp_path / "dump.json"
        path.write_text(json.dumps(dump))
        corpus = bench.corpus_from_dump(path)
        assert [c["content"] for c in corpus] == ["first", "article"]
        assert corpus[0]["tags"] == ["x", "y", "bench"]
        assert corpus[1]["tags"] == ["bench"]
        assert corpus[0]["namespace"] == "episodic"
        assert bench.corpus_from_dump(path, size=1) == corpus[:1]

    def test_from_dump_accepts_bare_data_mapping(self, tmp_path):
        path = tmp_path / "dump.json"
        path.write_text(json.dumps({"mem:episodic:01A": {"content": "bare"}}))
        assert bench.corpus_from_dump(path)[0]["content"] == "bare"

    def test_queries_come_from_the_corpus(self):
        corpus = bench.synthetic_corpus(30)
        queries = bench.derive_queries(corpus, 10)
        assert len(queries) == 14  # 10 from the corpus + 4 generic
        assert "why did the build fail on arm64" in queries
        corpus_text = " ".join(c["content"] for c in corpus)
        assert all(q in corpus_text for q in queries[:10])

    def test_short_text_query_is_the_whole_text(self):
        queries = bench.derive_queries([{"content": "short one", "namespace": "episodic", "tags": []}], 1)
        assert queries[0] == "short one"


class TestRun:
    def test_end_to_end_against_fakes(self, fake_store, fake_embedder):
        corpus = bench.synthetic_corpus(24, seed=1)
        queries = bench.derive_queries(corpus, 6)
        report = bench.run_benchmark(
            fake_store, fake_embedder, corpus, queries,
            batch_sizes=(1, 8, 64), vector_sample=5, load_ms=12.5, label="fakes",
        )
        s = report["speed"]
        assert report["label"] == "fakes"
        assert s["model_load_ms"] == 12.5
        assert s["embed_single_ms"]["n"] == 24
        assert set(s["embed_batch"]) == {"1", "8"}  # 64 > corpus, skipped
        assert s["remember_ms"]["n"] == 24
        assert s["remember_document"]["chunks_stored"] >= 1
        assert s["recall_ms"]["n"] == 10 and s["recall_index_ms"]["n"] == 10
        assert s["peak_rss_mb"] > 0
        eq = report["equivalence"]
        assert len(eq["vector_sample"]) == 5
        assert eq["vector_dim"] == 384
        assert set(eq["top_k"]) == set(queries)
        assert all(all({"key", "score", "content"} <= set(e) for e in v) for v in eq["top_k"].values())
        # Everything the run wrote is gone again
        assert fake_store.scan_prefix("mem:") == []
        assert fake_store.scan_prefix("log:recall:") == []
        assert report["cleanup"]["deleted"] > 24

    def test_cleanup_leaves_pre_existing_records_alone(self, fake_store, fake_embedder):
        from tests.conftest import store_memory
        store_memory(fake_store, fake_embedder, "mem:episodic:KEEP", "keep me")
        fake_store.client.hset("log:recall:KEEP", mapping={"query": "x"})
        bench.run_benchmark(fake_store, fake_embedder, bench.synthetic_corpus(5), ["q"],
                            vector_sample=1)
        assert fake_store.scan_prefix("mem:") == ["mem:episodic:KEEP"]
        assert "log:recall:KEEP" in fake_store.scan_prefix("log:recall:")

    def test_format_report(self, fake_store, fake_embedder):
        report = bench.run_benchmark(fake_store, fake_embedder, bench.synthetic_corpus(8), ["q"],
                                     vector_sample=1, load_ms=3.0)
        text = bench.format_report(report)
        assert "| model load | 3.0 ms |" in text
        assert "recall() p50" in text


def _report(label, vec_scale=1.0, top=None, dim=384, load=100.0, p50=2.0, batch=200.0):
    vec = [vec_scale * (i + 1) for i in range(dim)]
    return {
        "report_version": 1, "label": label, "backend": {"torch": "2.4"},
        "speed": {
            "model_load_ms": load, "peak_rss_mb": 900.0,
            "embed_single_ms": {"p50": p50, "p95": p50 * 2},
            "embed_batch": {"8": {"texts_per_s": batch}, "32": {"texts_per_s": batch * 2}},
            "remember_ms": {"p50": 5.0, "p95": 9.0},
            "remember_document": {"ms_per_chunk": 4.0},
            "recall_ms": {"p50": 6.0, "p95": 11.0},
            "recall_index_ms": {"p50": 6.5},
        },
        "equivalence": {
            "vector_dim": dim,
            "vector_sample": [{"text": "alpha", "vector": vec}, {"text": "beta", "vector": vec}],
            "top_k": top if top is not None else {
                "q1": [{"key": "k1", "score": 0.9, "content": "A"}, {"key": "k2", "score": 0.8, "content": "B"}],
            },
        },
    }


class TestCompare:
    def test_identical_runs_are_equivalent(self):
        cmp = bench.compare(_report("a"), _report("b"))
        assert cmp["equivalence"]["vector_cosine"]["min"] == 1.0
        assert cmp["equivalence"]["top_k"]["mean_jaccard"] == 1.0
        assert cmp["equivalence"]["top_k"]["top1_identical_pct"] == 100.0
        assert cmp["warnings"] == []
        assert cmp["speed"]["embed_single_p50_ms"]["change_pct"] == 0.0

    def test_speed_deltas_and_direction(self):
        cmp = bench.compare(_report("torch", load=1000, p50=4.0, batch=100),
                            _report("onnx", load=250, p50=1.0, batch=400))
        assert cmp["speed"]["model_load_ms"]["change_pct"] == -75.0
        assert cmp["speed"]["embed_batch_8_texts_per_s"]["change_pct"] == 300.0
        text = bench.format_comparison(cmp)
        assert "| model_load_ms | 1000 | 250 | -75.0% (better) |" in text
        assert "| embed_batch_8_texts_per_s | 100 | 400 | +300.0% (better) |" in text

    def test_scaled_vectors_are_still_equivalent_but_rotated_ones_are_not(self):
        # Scaling doesn't change direction: cosine 1.0
        cmp = bench.compare(_report("a"), _report("b", vec_scale=3.0))
        assert cmp["equivalence"]["vector_cosine"]["min"] == 1.0
        # A genuinely different vector trips the warning
        after = _report("b")
        after["equivalence"]["vector_sample"][0]["vector"] = [1.0] + [0.0] * 383
        cmp = bench.compare(_report("a"), after)
        assert cmp["equivalence"]["vector_cosine"]["below_0_99"] == 1
        assert any("not equivalent" in w for w in cmp["warnings"])

    def test_dimension_change_warns(self):
        cmp = bench.compare(_report("a"), _report("b", dim=768))
        assert any("dimension changed" in w for w in cmp["warnings"])
        assert "vector_cosine" not in cmp["equivalence"]

    def test_top_k_drift_matched_by_content(self):
        after = _report("b", top={
            "q1": [{"key": "NEW2", "score": 0.85, "content": "B"}, {"key": "NEW3", "score": 0.7, "content": "C"}],
        })
        cmp = bench.compare(_report("a"), after)
        tk = cmp["equivalence"]["top_k"]
        assert tk["mean_jaccard"] == round(1 / 3, 4)
        assert tk["top1_identical"] == 0
        assert tk["score_abs_delta_max"] == round(abs(0.85 - 0.8), 6)
        assert any("top-k overlap" in w for w in cmp["warnings"])

    def test_old_report_with_narrower_content_key_still_compares(self):
        before = _report("a", top={"q1": [{"key": "k1", "score": 0.9, "content": "A" * 80}]})
        after = _report("b", top={"q1": [{"key": "NEW", "score": 0.9, "content": "A" * 200}]})
        cmp = bench.compare(before, after)
        assert cmp["equivalence"]["top_k"]["mean_jaccard"] == 1.0
        assert cmp["equivalence"]["top_k"]["top1_identical"] == 1

    def test_no_shared_material(self):
        after = _report("b", top={})
        after["equivalence"]["vector_sample"] = []
        cmp = bench.compare(_report("a"), after)
        assert "vector_cosine" not in cmp["equivalence"]
        assert "top_k" not in cmp["equivalence"]
        assert "No shared equivalence material" in bench.format_comparison(cmp)


class TestCli:
    def test_compare_command_writes_and_exits_by_warnings(self, tmp_path, capsys):
        before, after = tmp_path / "b.json", tmp_path / "a.json"
        before.write_text(json.dumps(_report("a")))
        after.write_text(json.dumps(_report("b")))
        out = tmp_path / "out" / "cmp.json"
        assert bench.main(["compare", str(before), str(after), "--out", str(out)]) == 0
        assert json.loads(out.read_text())["warnings"] == []
        assert "Embedding benchmark: before vs after" in capsys.readouterr().out
        after.write_text(json.dumps(_report("b", dim=768)))
        assert bench.main(["compare", str(before), str(after)]) == 1

    def test_run_command_with_fakes(self, tmp_path, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setattr(bench, "_connect_real", lambda allow: (fake_store, fake_embedder, 7.0))
        out = tmp_path / "r" / "report.json"
        assert bench.main(["run", "--out", str(out), "--size", "10", "--queries", "3",
                           "--label", "fakes"]) == 0
        report = json.loads(out.read_text())
        assert report["label"] == "fakes" and report["speed"]["model_load_ms"] == 7.0
        assert report["corpus"]["source"] == "synthetic(seed=7)"

    def test_run_command_with_dump_corpus(self, tmp_path, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setattr(bench, "_connect_real", lambda allow: (fake_store, fake_embedder, 1.0))
        dump = tmp_path / "dump.json"
        dump.write_text(json.dumps({"data": {
            f"mem:episodic:{i:02d}": {"content": f"memory number {i} about topic {i % 3}"}
            for i in range(12)
        }}))
        out = tmp_path / "report.json"
        assert bench.main(["run", "--out", str(out), "--corpus", str(dump), "--queries", "2"]) == 0
        assert json.loads(out.read_text())["corpus"]["source"] == str(dump)

    def test_run_refuses_empty_corpus(self, tmp_path):
        dump = tmp_path / "dump.json"
        dump.write_text(json.dumps({"data": {}}))
        with pytest.raises(SystemExit, match="corpus is empty"):
            bench.main(["run", "--out", str(tmp_path / "r.json"), "--corpus", str(dump)])

    def test_connect_real_refuses_populated_store(self, fake_store, fake_embedder, monkeypatch):
        from tests.conftest import store_memory
        import memory.store as store_module
        import memory.embedder as embedder_module
        store_memory(fake_store, fake_embedder, "mem:episodic:X", "existing")
        monkeypatch.setattr(store_module, "ValkeyStore", lambda: fake_store)
        monkeypatch.setattr(embedder_module, "Embedder", lambda: fake_embedder)
        with pytest.raises(SystemExit, match="already holds 1 memories"):
            bench._connect_real(allow_existing=False)
        fake_embedder.load = lambda: None
        store, emb, load_ms = bench._connect_real(allow_existing=True)
        assert store is fake_store and load_ms >= 0
