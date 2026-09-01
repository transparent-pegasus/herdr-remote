import type { Card, Page } from "./transcript";

export type Pane = {
	id: string;
	label: string;
	agent: string | null;
	state: string;
};

export type Tab = { id: string; label: string; panes: Pane[] };

export type Workspace = { id: string; label: string; tabs: Tab[] };

export type Session = { workspaces: Workspace[] };

export type StateIndicator = "transparent" | "gray" | "green" | "yellow";

export function stateIndicator(states: Iterable<string>): StateIndicator {
	let indicator: StateIndicator = "transparent";
	for (const state of states) {
		if (state === "blocked") return "yellow";
		if (state === "working") indicator = "green";
		else if (
			(state === "idle" || state === "done") &&
			indicator === "transparent"
		) {
			indicator = "gray";
		}
	}
	return indicator;
}

/** The server's error bodies are short and human-readable; show them. */
async function reason(response: Response, fallback: string): Promise<string> {
	const body = (await response.text().catch(() => "")).trim();
	return body ? body.slice(0, 200) : `${fallback} (${response.status})`;
}

function isSession(value: unknown): value is Session {
	return (
		typeof value === "object" &&
		value !== null &&
		Array.isArray((value as Session).workspaces)
	);
}

export async function fetchSession(signal?: AbortSignal): Promise<Session> {
	const response = await fetch("/api/session", { signal });
	if (!response.ok) {
		throw new Error(await reason(response, "could not load session"));
	}
	const body: unknown = await response.json();
	// A proxy or a future schema slip should not surface as "reading 'length'".
	if (!isSession(body)) {
		throw new Error("unexpected response from /api/session");
	}
	return body;
}

const paneUrl = (paneId: string, rest: string) =>
	`/api/panes/${encodeURIComponent(paneId)}/${rest}`;

/** Every write is the same request modulo path and body; only the fallback
 *  message differs, and the server's own text wins when it sends one. */
async function post(
	url: string,
	fallback: string,
	body?: unknown,
	signal?: AbortSignal,
): Promise<void> {
	const init: RequestInit = { method: "POST", signal };
	if (body !== undefined) {
		init.headers = { "content-type": "application/json" };
		init.body = JSON.stringify(body);
	}
	const response = await fetch(url, init);
	if (!response.ok) {
		throw new Error(await reason(response, fallback));
	}
}

export const sendPrompt = (paneId: string, text: string): Promise<void> =>
	post(paneUrl(paneId, "prompt"), "could not send", { text });

/** Esc to a pane that is mid-turn. */
export const interruptPane = (paneId: string): Promise<void> =>
	post(paneUrl(paneId, "interrupt"), "could not interrupt");

/** Enter to a pane, for the question an agent is already asking. */
export const pressEnter = (paneId: string): Promise<void> =>
	post(paneUrl(paneId, "enter"), "could not send enter");

/** Up and down move the selection in the menu an agent is showing; the key
 *  name doubles as the route, as it does for the other bare keys. */
export const pressArrow = (paneId: string, key: "up" | "down"): Promise<void> =>
	post(paneUrl(paneId, key), `could not send ${key}`);

export type Output = { text: string; truncated: boolean };

/** Plain text, straight from the pane; the caller renders it verbatim.
 *  `truncated` says herdr held more than `lines` asked for. */
export async function fetchOutput(
	paneId: string,
	lines: number,
	source: "scrollback" | "screen",
	signal?: AbortSignal,
): Promise<Output> {
	const response = await fetch(
		`${paneUrl(paneId, "output")}?lines=${lines}&source=${source}`,
		{ signal },
	);
	if (!response.ok) {
		throw new Error(await reason(response, "could not read the pane"));
	}
	return {
		text: await response.text(),
		truncated: response.headers.get("x-truncated") === "true",
	};
}

/** Agent panes show what they are and what they are doing; shells just say so. */
export function paneSubtitle(pane: Pane): string {
	return pane.agent ? `${pane.agent} · ${pane.state}` : "shell";
}

/** Which pane is being read, in the one place that is always on screen. A
 *  shell has nothing to prefix, and the state is left to the indicators —
 *  the hammer, the stop button's own icon — rather than said in words. */
export function paneTitle(pane: Pane): string {
	return pane.agent ? `${pane.agent} · ${pane.label}` : pane.label;
}

/** The stop button interrupts an active turn and clears an inactive one. */
export function stopPresentation(state: string) {
	const interrupts = state === "working";
	return {
		label: interrupts ? "Stop" : "/clear",
		icon: interrupts ? "hand" : "eraser",
		interrupts,
	} as const;
}

/** A timed-out or dropped request is expected here rather than exceptional: the
 *  phone sleeps, WARP reconnects, the tunnel blips. Callers retry instead of
 *  surfacing "signal timed out", which reads as a fault the user must act on. */
export function isTransient(error: unknown): boolean {
	return (
		(error instanceof DOMException && error.name === "TimeoutError") ||
		// fetch() rejects with a TypeError when the connection itself fails.
		error instanceof TypeError
	);
}

export type Live = { screen: string; composer: string };

/** The newest-window ETag per pane. Cleared when a pane stops having a
 *  transcript, so a later attach starts from a real request. */
const etags = new Map<string, string>();

export function forgetPane(paneId: string): void {
	etags.delete(paneId);
}

function isCard(value: unknown): value is Card {
	const card = value as Card;
	return (
		typeof value === "object" &&
		value !== null &&
		typeof card.seq === "number" &&
		(card.role === "user" || card.role === "assistant") &&
		typeof card.preview === "string" &&
		typeof card.html === "string"
	);
}

function isPage(value: unknown): value is Page {
	const body = value as Page;
	return (
		typeof value === "object" &&
		value !== null &&
		Array.isArray(body.messages) &&
		body.messages.every(isCard) &&
		typeof body.has_more === "boolean"
	);
}

/** `null` means the pane has no transcript — a shell, or an agent whose session
 *  file could not be resolved — and the caller falls back to raw output.
 *  `"unchanged"` is the 304 that keeps a quiet poll free. */
export async function fetchTranscript(
	paneId: string,
	before?: number,
	signal?: AbortSignal,
): Promise<Page | "unchanged" | null> {
	const newest = before === undefined;
	const url = `${paneUrl(paneId, "transcript")}?limit=30${newest ? "" : `&before=${before}`}`;
	const etag = newest ? etags.get(paneId) : undefined;
	const response = await fetch(url, {
		signal,
		headers: etag ? { "if-none-match": etag } : undefined,
	});
	if (response.status === 404) {
		etags.delete(paneId);
		return null;
	}
	if (response.status === 304) return "unchanged";
	if (!response.ok) {
		throw new Error(await reason(response, "could not load the transcript"));
	}
	const tag = response.headers.get("etag");
	const body: unknown = await response.json();
	if (!isPage(body)) {
		throw new Error("unexpected response from the transcript route");
	}
	if (newest && tag) etags.set(paneId, tag);
	return body;
}

function isLive(value: unknown): value is Live {
	const live = value as Live;
	return (
		typeof value === "object" &&
		value !== null &&
		typeof live.screen === "string" &&
		typeof live.composer === "string"
	);
}

/** The terminal's own screen, plus the first line of whatever is typed into the
 *  agent's box. Both are width-dependent; the transcript is not. */
export async function fetchLive(
	paneId: string,
	signal?: AbortSignal,
): Promise<Live> {
	const response = await fetch(paneUrl(paneId, "live"), { signal });
	if (!response.ok) {
		throw new Error(await reason(response, "could not read the pane"));
	}
	const body: unknown = await response.json();
	if (!isLive(body)) {
		throw new Error("unexpected response from the live route");
	}
	return body;
}

export async function fetchAgentTick(
	paneId: string,
	timeout: () => AbortSignal,
	current: () => boolean,
	liveSettled: (live: Live | undefined) => void,
): Promise<{
	live: Live | undefined;
	liveError: unknown;
	page: Page | "unchanged" | null;
} | null> {
	let live: Live | undefined;
	let liveError: unknown;
	try {
		live = await fetchLive(paneId, timeout());
	} catch (error) {
		liveError = error;
	}
	if (!current()) return null;
	liveSettled(live);
	const page = await fetchTranscript(paneId, undefined, timeout());
	if (!current()) return null;
	return { live, liveError, page };
}

/** Zooming gives the pane the whole herdr window: a picker rendered into twenty
 *  columns is destroyed before it is ever read. How wide that ends up being is
 *  the operator's window, not something this code predicts. */
export const openPane = (paneId: string): Promise<void> =>
	post(
		paneUrl(paneId, "open"),
		"could not open the pane",
		undefined,
		AbortSignal.timeout(5000),
	);

/** `keepalive`, because this also fires from `pagehide`, when ordinary requests
 *  are cancelled along with the page. A close that is lost anyway is repaired
 *  by the server's next `open`, so nothing here needs to be awaited. */
export function closePane(paneId: string): void {
	void fetch(paneUrl(paneId, "close"), {
		method: "POST",
		keepalive: true,
	}).catch(() => {});
}

export type { Card, Page };
