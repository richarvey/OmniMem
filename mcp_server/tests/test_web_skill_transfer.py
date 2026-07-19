"""Web UI skill export/import routes, driven through the real Starlette app."""

import io
import sys
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent))

from tests.test_skill_transfer import _seed_skill

_SKILL_KEY = "mem:skill:gen:python-local"
_ALL_KEYS = (
    _SKILL_KEY, "mem:episodic:01A", "mem:episodic:01B", "mem:knowledge:01C",
)


def _export_zip(web_client) -> bytes:
    response = web_client.get(f"/skills/export/{_SKILL_KEY}")
    assert response.status_code == 200
    return response.content


def _upload(web_client, data: bytes, filename="bundle.zip"):
    return web_client.post(
        "/skills/import",
        files={"file": (filename, io.BytesIO(data), "application/zip")},
    )


def _token_from_preview(html: str) -> str:
    marker = 'name="token" value="'
    start = html.index(marker) + len(marker)
    return html[start:html.index('"', start)]


class TestExportRoute:
    def test_download_headers_and_content(self, web_client, fake_store, fake_embedder):
        _seed_skill(fake_store, fake_embedder)
        response = web_client.get(f"/skills/export/{_SKILL_KEY}")
        assert response.status_code == 200
        assert response.headers["content-type"] == "application/zip"
        assert "attachment" in response.headers["content-disposition"]
        assert ".zip" in response.headers["content-disposition"]
        with zipfile.ZipFile(io.BytesIO(response.content)) as zf:
            assert "manifest.json" in zf.namelist()

    def test_unknown_skill_404(self, web_client):
        response = web_client.get("/skills/export/mem:skill:gen:nope-local")
        assert response.status_code == 404

    def test_non_skill_key_404(self, web_client, fake_store, fake_embedder):
        _seed_skill(fake_store, fake_embedder)
        response = web_client.get("/skills/export/mem:episodic:01A")
        assert response.status_code == 404


class TestImportRoute:
    def test_missing_file_rejected(self, web_client):
        response = web_client.post("/skills/import", data={})
        assert response.status_code == 200
        assert "Choose a .zip bundle" in response.text

    def test_wrong_extension_rejected(self, web_client):
        response = _upload(web_client, b"whatever", filename="bundle.tar.gz")
        assert "Only .zip bundles" in response.text

    def test_corrupt_zip_rejected(self, web_client):
        response = _upload(web_client, b"this is not a zip")
        assert "Not a valid zip file" in response.text

    def test_full_roundtrip_restores_deleted_skill(
        self, web_client, fake_store, fake_embedder,
    ):
        _seed_skill(fake_store, fake_embedder)
        data = _export_zip(web_client)

        for key in _ALL_KEYS:
            fake_store.delete(key)

        preview = _upload(web_client, data)
        assert preview.status_code == 200
        assert "Bundle validated" in preview.text
        assert "Skill will be created" in preview.text
        assert "3 source" in preview.text
        assert "Confirm Import" in preview.text

        token = _token_from_preview(preview.text)
        confirm = web_client.post(
            "/skills/import/confirm", data={"token": token},
            follow_redirects=False,
        )
        assert confirm.status_code == 200
        assert confirm.headers["HX-Redirect"].startswith("/skills?message=")

        assert fake_store.get(_SKILL_KEY)["generated"] == "true"
        for key in _ALL_KEYS[1:]:
            assert fake_store.get(key) is not None
        # Re-embedded on this instance during import.
        assert "vector" in fake_store.client._data["mem:episodic:01A"]

    def test_import_into_populated_store_is_noop(
        self, web_client, fake_store, fake_embedder,
    ):
        _seed_skill(fake_store, fake_embedder)
        data = _export_zip(web_client)

        preview = _upload(web_client, data)
        assert "already exists" in preview.text
        assert "confirming would" in preview.text
        assert "Confirm Import" not in preview.text

    def test_partial_import_skips_existing(
        self, web_client, fake_store, fake_embedder,
    ):
        _seed_skill(fake_store, fake_embedder)
        data = _export_zip(web_client)
        fake_store.delete("mem:episodic:01A")

        original = fake_store.get("mem:episodic:01B")["content"]
        preview = _upload(web_client, data)
        assert "already exists" in preview.text  # skill untouched
        assert "1 source" in preview.text  # one memory to add
        assert "already stored here" in preview.text

        token = _token_from_preview(preview.text)
        confirm = web_client.post(
            "/skills/import/confirm", data={"token": token},
            follow_redirects=False,
        )
        assert confirm.status_code == 200
        assert fake_store.get("mem:episodic:01A") is not None
        assert fake_store.get("mem:episodic:01B")["content"] == original

    def test_confirm_with_bad_token(self, web_client):
        response = web_client.post("/skills/import/confirm", data={"token": "!!"})
        assert "Invalid import token" in response.text

    def test_confirm_with_expired_token(self, web_client):
        response = web_client.post(
            "/skills/import/confirm", data={"token": "a" * 32},
        )
        assert "expired" in response.text

    def test_confirm_token_is_one_shot(
        self, web_client, fake_store, fake_embedder,
    ):
        _seed_skill(fake_store, fake_embedder)
        data = _export_zip(web_client)
        for key in _ALL_KEYS:
            fake_store.delete(key)

        token = _token_from_preview(_upload(web_client, data).text)
        first = web_client.post(
            "/skills/import/confirm", data={"token": token},
            follow_redirects=False,
        )
        assert "HX-Redirect" in first.headers
        second = web_client.post(
            "/skills/import/confirm", data={"token": token},
        )
        assert "expired" in second.text


class TestSkillsPagesCarryTransferUI:
    def test_list_page_has_import_and_export(
        self, web_client, fake_store, fake_embedder,
    ):
        _seed_skill(fake_store, fake_embedder)
        response = web_client.get("/skills")
        assert "Import Skill" in response.text
        assert f"/skills/export/{_SKILL_KEY}" in response.text

    def test_list_page_shows_flash_message(self, web_client):
        response = web_client.get("/skills", params={"message": "Skill imported."})
        assert "Skill imported." in response.text

    def test_detail_page_has_export(self, web_client, fake_store, fake_embedder):
        _seed_skill(fake_store, fake_embedder)
        response = web_client.get(f"/skills/{_SKILL_KEY}")
        assert f"/skills/export/{_SKILL_KEY}" in response.text
