export type Route = { tabId?: string; paneId?: string };

/** `/`, `/t/<tab>`, `/t/<tab>/p/<pane>`. Anything else reads as the root. */
export function parseRoute(pathname: string): Route {
	let parts: string[];
	// A hand-typed URL can carry a stray `%`, and a throw here would blank the page.
	try {
		parts = pathname.split("/").filter(Boolean).map(decodeURIComponent);
	} catch {
		return {};
	}
	const [t, tabId, p, paneId] = parts;
	if (t !== "t" || !tabId) return {};
	if (p !== "p" || !paneId) return { tabId };
	return { tabId, paneId };
}

export function href(route: Route): string {
	if (!route.tabId) return "/";
	const tab = `/t/${encodeURIComponent(route.tabId)}`;
	return route.paneId ? `${tab}/p/${encodeURIComponent(route.paneId)}` : tab;
}
