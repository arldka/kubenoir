/** Commands the terminal answers itself, without asking the engine. */

export const BUILTINS = ['clear', 'help'] as const;

const HELP = `This is a simulated cluster. These commands work:

  kubectl get pods|deployments|replicasets|events [NAME] [-n NS] [-o wide|json|yaml|name]
  kubectl describe pod|deployment [NAME]
  kubectl logs POD|deploy/NAME [-c CONTAINER] [-p] [--tail N] [--timestamps]
  kubectl set env deployment/NAME KEY=VALUE ... | KEY-   (--list shows the environment)
  kubectl rollout restart deployment/NAME
  kubectl delete pod|deployment|replicaset NAME [--force]
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
