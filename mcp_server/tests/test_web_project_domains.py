"""Web UI tests for project work-type domains (v6.6)."""

import time

import pytest

from memory import project_domains as pd


def seed_project(store, embedder, name="webproj", domains=None,
                 stack="python, valkey", state="active"):
    now = str(time.time())
    fields = {
        "content": "a test project", "project_name": name,
        "description": "a test project", "stack": stack,
        "goals": "ship v6", "current_state": "in progress",
        "notes": "", "state": state,
        "surface_score": "1.0", "created_at": now, "updated_at": now,
    }
    if domains is not None:
        fields["domains"] = pd.serialise_domains(domains)
    store.upsert("project", f"mem:project:{name}", fields, embedder.embed("a test project"))
    pd.invalidate_domain_cache()


def seed_skill(store, embedder, domain, user="local"):
    now = str(time.time())
    store.upsert("skill", f"mem:skill:gen:{domain}-{user}", {
        "name": f"{domain}-{user}", "description": f"{domain} skill",
        "domain": domain, "user": user, "generated": "true",
        "body": "# skill", "state": "active", "surface_score": "1.0",
        "created_at": now, "updated_at": now, "compiled_at": now,
        "contract_version": "1",
    }, embedder.embed(domain))


class TestProjectListDomains:
    def test_domains_render_as_pills(self, web_client, fake_store, fake_embedder):
        seed_project(fake_store, fake_embedder, domains=["python", "docker"])
        resp = web_client.get("/projects")
        assert resp.status_code == 200
        assert 'href="/projects?domain=python"' in resp.text
        assert 'href="/projects?domain=docker"' in resp.text

    def test_filter_chips_show_counts(self, web_client, fake_store, fake_embedder):
        seed_project(fake_store, fake_embedder, "a", domains=["python"])
        seed_project(fake_store, fake_embedder, "b", domains=["python"])
        resp = web_client.get("/projects")
        assert "domain-filter-bar" in resp.text
        assert "domain-chip" in resp.text

    def test_domain_filter_narrows_the_list(self, web_client, fake_store, fake_embedder):
        seed_project(fake_store, fake_embedder, "pyproj", domains=["python"])
        seed_project(fake_store, fake_embedder, "designproj", domains=["design"])
        resp = web_client.get("/projects?domain=python")
        assert "pyproj" in resp.text
        assert "designproj" not in resp.text

    def test_alias_in_the_query_string_resolves(self, web_client, fake_store, fake_embedder):
        seed_project(fake_store, fake_embedder, "pyproj", domains=["python"])
        assert "pyproj" in web_client.get("/projects?domain=py").text

    def test_unmatched_domain_explains_itself(self, web_client, fake_store, fake_embedder):
        seed_project(fake_store, fake_embedder, "pyproj", domains=["python"])
        resp = web_client.get("/projects?domain=rust")
        assert resp.status_code == 200
        assert "No project declares the domain" in resp.text

    def test_unusable_domain_is_reported_not_crashed(self, web_client, fake_store, fake_embedder):
        seed_project(fake_store, fake_embedder, domains=["python"])
        resp = web_client.get("/projects?domain=%3Cscript%3E")
        assert resp.status_code == 200
        assert "is not a usable domain" in resp.text

    def test_project_without_domains_shows_a_dash(self, web_client, fake_store, fake_embedder):
        seed_project(fake_store, fake_embedder, domains=[])
        assert web_client.get("/projects").status_code == 200

    def test_corrupt_timestamp_still_renders(self, web_client, fake_store, fake_embedder):
        seed_project(fake_store, fake_embedder, domains=["python"])
        fake_store.set_field("mem:project:webproj", "updated_at", "not-a-number")
        assert web_client.get("/projects").status_code == 200


class TestProjectDetailDomains:
    def test_pills_and_hint(self, web_client, fake_store, fake_embedder):
        seed_project(fake_store, fake_embedder, domains=["python"])
        resp = web_client.get("/projects/webproj")
        assert 'href="/projects?domain=python"' in resp.text
        assert "domain_filter=" in resp.text

    def test_links_to_a_compiled_skill_when_one_exists(
        self, web_client, fake_store, fake_embedder
    ):
        seed_project(fake_store, fake_embedder, domains=["python"])
        seed_skill(fake_store, fake_embedder, "python")
        resp = web_client.get("/projects/webproj")
        assert "mem:skill:gen:python-local" in resp.text

    def test_no_skill_link_without_a_skill(self, web_client, fake_store, fake_embedder):
        seed_project(fake_store, fake_embedder, domains=["python"])
        resp = web_client.get("/projects/webproj")
        assert "mem:skill:gen:python" not in resp.text

    def test_empty_state_points_at_the_edit_page(
        self, web_client, fake_store, fake_embedder
    ):
        seed_project(fake_store, fake_embedder, domains=[])
        resp = web_client.get("/projects/webproj")
        assert "None set." in resp.text


class TestProjectEditDomains:
    def test_form_prefills_domains(self, web_client, fake_store, fake_embedder):
        seed_project(fake_store, fake_embedder, domains=["python", "docker"])
        resp = web_client.get("/projects/webproj/edit")
        assert 'value="python,docker"' in resp.text

    def test_datalist_offers_known_domains(self, web_client, fake_store, fake_embedder):
        seed_project(fake_store, fake_embedder, domains=["python"])
        seed_skill(fake_store, fake_embedder, "wcag-accessibility")
        resp = web_client.get("/projects/webproj/edit")
        assert "project-domain-options" in resp.text
        assert "wcag-accessibility" in resp.text

    def test_save_normalises_domains(self, web_client, fake_store, fake_embedder):
        seed_project(fake_store, fake_embedder)
        web_client.post("/projects/webproj/edit", data={
            "description": "d", "stack": "python", "goals": "g",
            "current_state": "s", "notes": "", "domains": "py, Docker",
        }, follow_redirects=False)
        assert fake_store.get("mem:project:webproj")["domains"] == "python,docker"

    def test_save_can_clear_domains(self, web_client, fake_store, fake_embedder):
        seed_project(fake_store, fake_embedder, domains=["python"])
        web_client.post("/projects/webproj/edit", data={
            "description": "d", "stack": "python", "goals": "g",
            "current_state": "s", "notes": "", "domains": "",
        }, follow_redirects=False)
        assert fake_store.get("mem:project:webproj")["domains"] == ""

    def test_save_drops_unusable_values(self, web_client, fake_store, fake_embedder):
        seed_project(fake_store, fake_embedder)
        web_client.post("/projects/webproj/edit", data={
            "description": "d", "stack": "python", "goals": "g",
            "current_state": "s", "notes": "", "domains": "python, <script>",
        }, follow_redirects=False)
        assert fake_store.get("mem:project:webproj")["domains"] == "python"

    def test_save_invalidates_the_resolution_cache(
        self, web_client, fake_store, fake_embedder
    ):
        seed_project(fake_store, fake_embedder, domains=[])
        assert pd.domain_map(fake_store) == {}
        web_client.post("/projects/webproj/edit", data={
            "description": "d", "stack": "python", "goals": "g",
            "current_state": "s", "notes": "", "domains": "python",
        }, follow_redirects=False)
        assert pd.domain_map(fake_store)["python"] == ["webproj"]

    def test_create_stores_domains(self, web_client, fake_store):
        web_client.post("/projects/new", data={
            "name": "brandnew", "description": "d", "stack": "s", "goals": "g",
            "current_state": "cs", "notes": "", "domains": "python",
        }, follow_redirects=False)
        assert fake_store.get("mem:project:brandnew")["domains"] == "python"

    def test_create_form_renders(self, web_client, fake_store, fake_embedder):
        seed_project(fake_store, fake_embedder, domains=["python"])
        resp = web_client.get("/projects/new")
        assert resp.status_code == 200
        assert "project-domain-options" in resp.text

    def test_new_form_has_no_suggest_button(self, web_client):
        # Nothing to read evidence from before the project exists.
        assert "domains/suggest" not in web_client.get("/projects/new").text


class TestDomainSuggestionEndpoint:
    def test_suggests_from_stack(self, web_client, fake_store, fake_embedder):
        seed_project(fake_store, fake_embedder, stack="Python and Docker")
        resp = web_client.post("/projects/webproj/domains/suggest")
        assert resp.status_code == 200
        assert "python" in resp.text
        assert "docker" in resp.text
        assert "stack" in resp.text

    def test_returns_a_partial_not_a_page(self, web_client, fake_store, fake_embedder):
        seed_project(fake_store, fake_embedder, stack="Python")
        resp = web_client.post("/projects/webproj/domains/suggest")
        assert "<html" not in resp.text.lower()
        assert "domain-suggestion" in resp.text

    def test_proposes_without_writing(self, web_client, fake_store, fake_embedder):
        seed_project(fake_store, fake_embedder, stack="Python")
        web_client.post("/projects/webproj/domains/suggest")
        assert "domains" not in fake_store.get("mem:project:webproj")

    def test_nothing_to_suggest(self, web_client, fake_store, fake_embedder):
        seed_project(fake_store, fake_embedder, stack="", domains=[])
        resp = web_client.post("/projects/webproj/domains/suggest")
        assert "Nothing new to suggest" in resp.text

    def test_missing_project_404s(self, web_client):
        assert web_client.post("/projects/ghost/domains/suggest").status_code == 404

    def test_value_rides_in_a_data_attribute(self, web_client, fake_store, fake_embedder):
        seed_project(fake_store, fake_embedder, stack="Python")
        resp = web_client.post("/projects/webproj/domains/suggest")
        assert 'data-domains="python"' in resp.text
