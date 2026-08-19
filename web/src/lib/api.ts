export type Pane = {
	id: string;
	label: string;
	agent: string | null;
	state: string;
};

export type Tab = { id: string; label: string; panes: Pane[] };

export type Session = { tabs: Tab[] };

export async function fetchSession(): Promise<Session> {
	const response = await fetch("/api/session");
	if (!response.ok) {
		throw new Error(`could not load session (${response.status})`);
	}
	return (await response.json()) as Session;
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
		throw new Error(`could not send (${response.status})`);
	}
}

/** Agent panes show what they are and what they are doing; shells just say so. */
export function paneSubtitle(pane: Pane): string {
	return pane.agent ? `${pane.agent} · ${pane.state}` : "shell";
}
