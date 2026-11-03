// Cloudflare Pages serves 404.html for any path it can't find, with a real
// 404. But 404.html is also an ordinary file, which clean URLs serve at /404
// with a 200, and _redirects can't change a status. This answers /404 with
// the same page and a 404. Only /404 runs it: every other path stays static.
// Responses from a Function don't get the _headers rules, so noindex is set
// here as well.
export async function onRequest({ request, env }) {
  const page = await env.ASSETS.fetch(new URL("/404", request.url));
  const headers = new Headers(page.headers);
  headers.set("X-Robots-Tag", "noindex");
  return new Response(page.body, { status: 404, headers });
}
