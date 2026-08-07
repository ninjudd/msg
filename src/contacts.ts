/** Name lookup for handles, backed by the macOS Contacts database. */

import { execFileSync } from 'node:child_process';
import { existsSync, readdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { basename, dirname, join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';

const ADDRESS_BOOK = join(homedir(), 'Library', 'Application Support', 'AddressBook');

/** Digits kept when matching phone numbers, which covers NANP numbers in full. */
const MATCH_DIGITS = 10;

export interface ContactIndex {
  /** The name for a handle, or null when it is unknown. */
  lookup(handle: string | null): string | null;
  /** How many handles the index knows about. */
  readonly size: number;
  /**
   * Why the index is emptier than expected, when it is.
   *
   * Contacts is best-effort — messages still read without it — but silently
   * best-effort is how an empty index went unnoticed until names stopped
   * resolving with no explanation anywhere.
   */
  readonly problems: readonly string[];
}

export const EMPTY_INDEX: ContactIndex = {
  lookup: () => null,
  size: 0,
  problems: [],
};

/**
 * Reduce a handle to a comparable key.
 *
 * Contacts stores numbers in whatever shape they were typed, so both sides are
 * stripped to digits. Numbers long enough to carry a country code are matched
 * on their final digits, which keeps `+13105551234` and `(310) 555-1234`
 * together.
 */
export function handleKey(handle: string): string | null {
  const trimmed = handle.trim();
  if (trimmed.length === 0) return null;
  if (trimmed.includes('@')) return trimmed.toLowerCase();

  const digits = trimmed.replace(/\D/g, '');
  if (digits.length === 0) return null;
  return digits.length > MATCH_DIGITS ? digits.slice(-MATCH_DIGITS) : digits;
}

/**
 * The Contacts source macOS treats as primary.
 *
 * Several accounts can hold a record for the same number under different
 * names, so the account the user actually writes to decides the winner.
 */
export function defaultSourceId(): string | null {
  try {
    const value = execFileSync(
      'defaults',
      ['read', 'com.apple.AddressBook', 'ABDefaultSourceID'],
      { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] },
    ).trim();
    return value.length > 0 ? value : null;
  } catch {
    return null;
  }
}

/** The source identifier a database belongs to, taken from its directory. */
function sourceIdOf(path: string): string | null {
  const parent = basename(dirname(path));
  return parent === 'AddressBook' ? null : parent;
}

/**
 * Every per-account database under `Sources/`.
 *
 * Walked with `readdirSync` rather than matched with a glob. A glob over this
 * path found nothing from inside the daemon while the very same files opened
 * fine when reached by directory walk, and a lookup that silently finds no
 * sources is indistinguishable from a machine with no contacts.
 */
function sourceDatabases(problems: string[]): string[] {
  const directory = join(ADDRESS_BOOK, 'Sources');
  let entries: string[];
  try {
    entries = readdirSync(directory);
  } catch (error) {
    problems.push(`${directory}: ${describe(error)}`);
    return [];
  }
  return entries
    .map((entry) => join(directory, entry, 'AddressBook-v22.abcddb'))
    .filter((path) => existsSync(path))
    .sort();
}

/** Every Contacts database: one per account, plus the legacy top-level one. */
function contactDatabases(problems: string[]): string[] {
  if (!existsSync(ADDRESS_BOOK)) return [];
  return [...sourceDatabases(problems), join(ADDRESS_BOOK, 'AddressBook-v22.abcddb')].filter(
    (path) => existsSync(path),
  );
}

function personName(row: Record<string, unknown>): string | null {
  const text = (key: string): string | null => {
    const value = row[key];
    return typeof value === 'string' && value.trim().length > 0 ? value.trim() : null;
  };
  const first = text('ZFIRSTNAME');
  const last = text('ZLASTNAME');
  if (first !== null || last !== null) return [first, last].filter((v) => v !== null).join(' ');
  return text('ZNICKNAME') ?? text('ZORGANIZATION');
}

/**
 * Read every Contacts source into a single handle-to-name map.
 *
 * A missing or unreadable database yields an empty index rather than an error,
 * so reading messages still works without Contacts access.
 */
export function loadContacts(preferredSource?: string | undefined): ContactIndex {
  const problems: string[] = [];

  if (!existsSync(ADDRESS_BOOK)) problems.push(`no Contacts directory at ${ADDRESS_BOOK}`);

  // Read every database before asking which source is preferred.
  //
  // The order matters, which is not obvious and cost a long afternoon. Asking
  // for `com.apple.AddressBook` preferences appears to make TCC start enforcing
  // the Contacts service against this process, and from then on these files are
  // refused with EPERM even though the process holds Full Disk Access. Two
  // builds differing only in whether the directory was read first behaved
  // differently: the one that read first saw 1,123 handles, the one that read
  // after saw none. Reading first costs nothing, since the preference only
  // decides which source wins a tie.
  const databases = contactDatabases(problems);
  if (databases.length === 0 && problems.length === 0) {
    problems.push(`no Contacts databases under ${ADDRESS_BOOK}`);
  }
  const sources = databases.map((path) => ({ path, names: readSource(path, problems) }));

  const preferred = preferredSource ?? process.env['MSG_CONTACTS_SOURCE'] ?? defaultSourceId();
  const names = new Map<string, string>();
  for (const source of [
    ...sources.filter((source) => sourceIdOf(source.path) === preferred),
    ...sources.filter((source) => sourceIdOf(source.path) !== preferred),
  ]) {
    // The preferred source is merged first, so its name for a handle wins.
    for (const [key, name] of source.names) if (!names.has(key)) names.set(key, name);
  }

  return {
    size: names.size,
    problems,
    lookup(handle) {
      if (handle === null) return null;
      const key = handleKey(handle);
      return key === null ? null : (names.get(key) ?? null);
    },
  };
}

function readSource(path: string, problems: string[]): Map<string, string> {
  const names = new Map<string, string>();
  try {
    const db = new DatabaseSync(path, { readOnly: true });
    collect(
      db,
      `SELECT p.ZFULLNUMBER AS handle, r.ZFIRSTNAME, r.ZLASTNAME, r.ZNICKNAME, r.ZORGANIZATION
         FROM ZABCDPHONENUMBER p
         JOIN ZABCDRECORD r ON r.Z_PK = p.ZOWNER
        WHERE p.ZFULLNUMBER IS NOT NULL`,
      names,
      problems,
      path,
    );
    collect(
      db,
      `SELECT e.ZADDRESS AS handle, r.ZFIRSTNAME, r.ZLASTNAME, r.ZNICKNAME, r.ZORGANIZATION
         FROM ZABCDEMAILADDRESS e
         JOIN ZABCDRECORD r ON r.Z_PK = e.ZOWNER
        WHERE e.ZADDRESS IS NOT NULL`,
      names,
      problems,
      path,
    );
    db.close();
  } catch (error) {
    // A source that cannot be opened is skipped; the others still count.
    problems.push(`${path}: ${describe(error)}`);
  }
  return names;
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function collect(
  db: DatabaseSync,
  sql: string,
  names: Map<string, string>,
  problems: string[],
  path: string,
): void {
  let rows: Array<Record<string, unknown>>;
  try {
    const statement = db.prepare(sql);
    statement.setReadBigInts(true);
    rows = statement.all() as Array<Record<string, unknown>>;
  } catch (error) {
    problems.push(`${path}: ${describe(error)}`);
    return;
  }

  for (const row of rows) {
    const handle = row['handle'];
    if (typeof handle !== 'string') continue;
    const key = handleKey(handle);
    const name = personName(row);
    if (key === null || name === null) continue;
    // Sources are visited primary first, so the first name for a handle is the
    // one from the account the user actually maintains.
    if (!names.has(key)) names.set(key, name);
  }
}

/** Replace handles with names where they are known, keeping order. */
export function nameHandles(contacts: ContactIndex, handles: string | null): string | null {
  if (handles === null) return null;
  return handles
    .split(',')
    .map((handle) => contacts.lookup(handle) ?? handle)
    .join(', ');
}
