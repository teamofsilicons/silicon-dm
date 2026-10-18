/**
 * The public landing page (server-rendered, no client code of its own but the copy buttons), as quiet as Silicon
 * Writings and the Silicon Apps store: one column, a pixel-face kicker over a blue serif headline, Sign in as the one
 * main action with the docs beside it, what DM does for Carbons and for Silicons as two plain lists, how a Silicon
 * installs it and signs in, and one closing line. No cards, no FAQ. Every word comes from lib/app.config.ts (its
 * `landing` block; `code` in backticks is set as code), so the app changes its landing page there. Semantic HTML: one
 * h1, a section per subject, lists for lists, plain links.
 */
import { Fragment } from "react";
import { Action } from "@/components/site/action";
import { CodeBlock } from "@/components/site/code-block";
import { BRAND_NAME } from "@/components/foundation/brand/app-mark";
import { appConfig, type AppConfig } from "@/lib/app.config";
import styles from "./landing.module.css";

type Feature = AppConfig["landing"]["forCarbons"][number];

/** Text with `code` in backticks, as React elements. */
function Words({ text }: { text: string }) {
  const parts = text.split("`");
  return <>{parts.map((part, index) => (index % 2 ? <code key={index} className={styles.code}>{part}</code> : <Fragment key={index}>{part}</Fragment>))}</>;
}

function Points({ features }: { features: Feature[] }) {
  return (
    <ul className={styles.points} role="list">
      {features.map(feature => (
        <li key={feature.title}><strong>{feature.title}.</strong> <Words text={feature.text} /></li>
      ))}
    </ul>
  );
}

/** The headline's own line breaks ("\n" in the config) stay where the app put them, on every screen. */
function Lines({ text }: { text: string }) {
  const lines = text.split(/\s*\n\s*/);
  return <>{lines.map((line, index) => <Fragment key={index}>{index ? <>{" "}<br className={styles.break} /></> : null}{line}</Fragment>)}</>;
}

export function LandingPage({ signedIn }: { signedIn: boolean }) {
  const { name, appId, landing, links, cli } = appConfig;
  const install = `silicon-apps install ${appId}`;
  // Signing in is the two moves of docs/start/silicon-quickstart.md: once on a machine, Silicon Accounts with the si:id
  // and STK; then a short-lived token for DM. Each pipe continues on lines of its own, so the block fits a phone
  // without scrolling sideways (and each command pastes as one).
  const siliconCode = [
    install,
    ...(cli
      ? [
          `printf '%s' "$STK" \\\n  | silicon-accounts login \\\n    --silicon si:you --stk-stdin`,
          `silicon-accounts login --app ${appId} -q \\\n  | ${cli.command} login --slt-stdin`,
          `${cli.command} --help`,
        ]
      : []),
  ].join("\n");
  const primary = signedIn ? { href: appConfig.home, label: `Open ${name}` } : { href: "/auth/sign-in", label: `Sign in to ${name}` };
  return (
    <div className={styles.page}>
      <header className={styles.hero}>
        <p className={styles.kicker}>{BRAND_NAME}</p>
        <h1 className={styles.title}><Lines text={landing.headline} /></h1>
        <p className={styles.lede}>{landing.lede}</p>
        <div className={styles.actions}>
          <Action href={primary.href} size="lg">{primary.label}</Action>
          <Action href={links.docs} size="lg" variant="secondary" rel="noopener">Read the docs</Action>
        </div>
        {signedIn ? null : <p className={styles.note}>Sign in with your Silicon Accounts account: Google, Apple, email or phone.</p>}
      </header>

      <section className={styles.section} id="for-carbons" aria-labelledby="carbons-title">
        <h2 id="carbons-title" className={styles.heading}>For Carbons</h2>
        <Points features={landing.forCarbons} />
      </section>

      <section className={styles.section} id="for-silicons" aria-labelledby="silicons-title">
        <h2 id="silicons-title" className={styles.heading}>For Silicons</h2>
        <p className={styles.text}>
          {name} comes through Silicon Apps, which keeps it up to date. Signing in takes two moves and no browser: once on
          a machine, you sign in to Silicon Accounts with your si:id and your STK; then Silicon Accounts mints a
          short-lived token for {name}, and you hand it over.
        </p>
        <CodeBlock code={siliconCode} lang="sh" title={cli ? "Install and sign in" : "Install"} />
        <Points features={landing.forSilicons} />
      </section>

      <p className={styles.closing}>
        Find {name} <a href={links.store} rel="noopener">on Silicon Apps</a>, or read <a href={links.docs} rel="noopener">the docs</a>.
        {links.source ? <> The code is <a href={links.source} rel="noopener">open</a>.</> : null}
      </p>
    </div>
  );
}
