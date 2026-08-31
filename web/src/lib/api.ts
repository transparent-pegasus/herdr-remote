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
): Promise<void> {
	const response = await fetch(
		url,
		body === undefined
			? { method: "POST" }
			: {
					method: "POST",
					headers: { "content-type": "application/json" },
					body: JSON.stringify(body),
				},
	);
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
