/** Commands the terminal answers itself, without asking the engine. */

export const BUILTINS = ['clear', 'help'] as const;

const HELP = `This is a simulated cluster. Start here:

  kubectl --help          the kubectl commands that work, grouped
  kubectl get --help      help for one command (every command takes --help)
  kubectl explain pod     what a resource's fields mean
  cat, ls, echo, grep, head, base64
                          shell commands; | pipes and > redirects work
  help, clear

Keys: Tab complete   Up/Down history   Ctrl+A/E start/end   Ctrl+U/K/W cut
      Ctrl+L clear   Ctrl+C cancel line`;

export type BuiltinResult = { output: string } | { clear: true };

/** The result of a built-in command, or null when `line` is for the engine. */
export function runBuiltin(line: string): BuiltinResult | null {
	switch (line.trim()) {
		case 'help':
			return { output: HELP };
		case 'clear':
			return { clear: true };
		default:
			return null;
	}
}

export function banner(title: string): string {
	return `CASE FILE: ${title}\nType 'help' for the commands you can use. Tab completes.\n`;
}
