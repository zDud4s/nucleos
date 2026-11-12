import { QueryClient } from "@tanstack/react-query";

/**
 * The one cache, with the house rules baked into its defaults.
 *
 * Every rule below is a *product* decision that would otherwise have to be
 * repeated at each of the ~40 hooks the shell will end up with — and repeated
 * decisions are decisions that drift.
 */
export function createAppQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: {
        /**
         * A hidden window does not poll.
         *
         * The shell lives in the tray for hours at a time, and a tray icon that
         * keeps the fleet at 3 s is a laptop fan for nobody's benefit. The two
         * queries that must keep going when hidden — the health probe and the
         * kill switch — say so for themselves, out loud, in `data/system.ts`.
         * Making the *quiet* case the default means the noisy ones have to
         * justify themselves.
         */
        refetchIntervalInBackground: false,

        /**
         * No retries on reads either.
         *
         * This looks aggressive and is not: polling *is* the retry. Every read
         * in this app is on an interval, so a failed fetch is already going to
         * be attempted again in 1.5–30 s. react-query's default three retries
         * with backoff would only delay the moment the shell admits it cannot
         * reach the daemon — which is the one thing the connection gate exists
         * to say quickly and honestly.
         */
        retry: false,
      },
      mutations: {
        /**
         * A refusal is settled, and a retried write is a second attempt at an
         * action a person asked for once. `POST /jobs` retried after a 409 is
         * two jobs if the first one actually landed.
         */
        retry: false,
      },
    },
  });
}

/**
 * `placeholderData: keepPreviousData` is deliberately *not* a default here.
 *
 * It is right for lists — a roster that blanks on every refetch makes the whole
 * shell flicker once every three seconds — and wrong for detail queries, where
 * it would show the previous run's output under the current run's title while
 * the new one loads. That is not a stale view, it is the wrong view. So the
 * lists opt in one by one (`useProjects`, `useProposals` in `data/system.ts`)
 * and details never do.
 */
