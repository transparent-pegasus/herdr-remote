export type Pane = {
	id: string;
	label: string;
	agent: string | null;
	state: string;
};

export type Tab = { id: string; label: string; panes: Pane[] };

export type Session = { tabs: Tab[] };

/** The server's error bodies are short and human-readable; show them. */
async function reason(response: Response, fallback: string): Promise<string> {
	const body = (await response.text().catch(() => "")).trim();
	return body ? body.slice(0, 200) : `${fallback} (${response.status})`;
}

function isSession(value: unknown): value is Session {
	return (
		typeof value === "object" &&
		value !== null &&
		Array.isArray((value as Session).tabs)
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

export async function sendPrompt(paneId: string, text: string): Promise<void> {
	const response = await fetch(
		`/api/panes/${encodeURIComponent(paneId)}/prompt`,
		{
			method: "POST",
			headers: { "content-type": "application/json" },
			body: JSON.stringify({ text }),
		},
	);
	if (!response.ok) {
		throw new Error(await reason(response, "could not send"));
	}
}

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
		`/api/panes/${encodeURIComponent(paneId)}/output?lines=${lines}&source=${source}`,
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
