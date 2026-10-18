/**
 * What a drop brings (lib/upload/drop.ts): plain files; folders walked by path with hidden files left out (and a cap
 * for a drop of a whole disk); a picture or link dragged from a web page attached as an https link, never fetched; a
 * picture inlined in a page (data:) taken as a file; everything else said in words, so a drop is never silently nothing.
 * And what the composer does with it (components/chat/composer/drop-plan.ts): what goes in at once, what is asked, what
 * is said and when. The browser's DataTransfer and its entries are faked with plain objects.
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import { planDrop } from "../components/chat/composer/drop-plan";
import { addressesOf, foldersWords, httpsLink, leftOutWords, pictureFromDataUrl, readDrop, snapshotDrop, type DataTransferLike, type EntryLike, type ItemLike } from "../lib/upload/drop";

const file = (name: string, text = name, type = "text/plain") => new File([text], name, { type });

/** A folder entry whose reader answers in batches of `batch` (as browsers do, 100 at a time). */
function folder(name: string, children: EntryLike[], batch = 2): EntryLike {
  return {
    isFile: false,
    isDirectory: true,
    name,
    createReader: () => {
      let at = 0;
      return {
        readEntries: success => {
          const next = children.slice(at, at + batch);
          at += next.length;
          setTimeout(() => success(next), 0);
        },
      };
    },
  };
}

const entry = (name: string): EntryLike => ({ isFile: true, isDirectory: false, name, file: success => setTimeout(() => success(file(name)), 0) });

function transfer(options: { items?: ItemLike[]; files?: File[]; data?: Record<string, string> }): DataTransferLike {
  const data = options.data ?? {};
  return {
    items: options.items,
    files: options.files ?? [],
    types: [...(options.items?.length || options.files?.length ? ["Files"] : []), ...Object.keys(data)],
    getData: format => data[format] ?? "",
  };
}

const fileItem = (value: File): ItemLike => ({ kind: "file", getAsFile: () => value, webkitGetAsEntry: () => entry(value.name) });
const folderItem = (value: EntryLike): ItemLike => ({ kind: "file", getAsFile: () => new File([], value.name), webkitGetAsEntry: () => value });

test("plain files, several at once, as dropped", async () => {
  const dropped = await readDrop(snapshotDrop(transfer({ items: [fileItem(file("a.txt")), fileItem(file("b.pdf", "%PDF", "application/pdf"))] })));
  assert.deepEqual(dropped.files.map(item => item.name), ["a.txt", "b.pdf"]);
  assert.deepEqual(dropped.folders, []);
  assert.deepEqual(dropped.links, []);
  assert.deepEqual(dropped.problems, []);
});

test("a folder: every file inside, by path, folders within folders, hidden files left out", async () => {
  const photos = folder("Photos", [entry("b.jpg"), entry(".DS_Store"), folder("2026", [entry("a 10.jpg"), entry("a 9.jpg")]), folder(".git", [entry("HEAD")]), entry("a.jpg")]);
  const dropped = await readDrop(snapshotDrop(transfer({ items: [folderItem(photos), fileItem(file("loose.txt"))] })));
  assert.deepEqual(dropped.files.map(item => item.name), ["loose.txt", "a 9.jpg", "a 10.jpg", "a.jpg", "b.jpg"], "numbers in names sort as numbers");
  assert.deepEqual(dropped.folders, [{ name: "Photos", files: 4, hidden: 2, truncated: false }]);
});

test("an empty folder and a folder that cannot be read are said, never silently nothing", async () => {
  const empty = await readDrop(snapshotDrop(transfer({ items: [folderItem(folder("Empty", [entry(".DS_Store")]))] })));
  assert.deepEqual(empty.folders, [{ name: "Empty", files: 0, hidden: 1, truncated: false }]);
  const locked: EntryLike = { isFile: false, isDirectory: true, name: "Locked", createReader: () => ({ readEntries: (_success, failure) => failure?.(new DOMException("denied", "SecurityError")) }) };
  const refused = await readDrop(snapshotDrop(transfer({ items: [folderItem(locked)] })));
  assert.deepEqual(refused.files, []);
  assert.match(refused.problems[0] ?? "", /The folder Locked could not be read/);
});

test("a drop of a huge tree stops at the limit and says the folder was cut short", async () => {
  const many = folder("Disk", Array.from({ length: 30 }, (_, index) => entry(`f${index}.txt`)), 7);
  const dropped = await readDrop(snapshotDrop(transfer({ items: [folderItem(many)] })), { limit: 10 });
  assert.equal(dropped.files.length, 10);
  assert.equal(dropped.folders[0]!.truncated, true);
});

test("a picture dragged from a web page: its https address as a link (never fetched); a link likewise", async () => {
  const picture = await readDrop(snapshotDrop(transfer({ data: { "text/uri-list": "https://example.com/page", "text/html": '<a href="https://example.com/page"><img alt="x" src="https://cdn.example.com/cat.png?w=800&amp;h=600"></a>' } })));
  assert.deepEqual(picture.links, ["https://cdn.example.com/cat.png?w=800&h=600"], "the picture's own address, not the page it linked to");
  assert.deepEqual(picture.files, []);
  const link = await readDrop(snapshotDrop(transfer({ data: { "text/uri-list": "# a comment\nhttps://example.com/notes" } })));
  assert.deepEqual(link.links, ["https://example.com/notes"]);
});

test("a picture inlined in a page (a data: address) becomes a file; other schemes are said", async () => {
  const png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
  const inline = await readDrop(snapshotDrop(transfer({ data: { "text/html": `<img src="data:image/png;base64,${png}">` } })), { now: new Date("2026-10-18T09:15:02Z") });
  assert.equal(inline.files.length, 1);
  assert.equal(inline.files[0]!.type, "image/png");
  assert.equal(inline.files[0]!.name, "Dropped picture 2026-10-18 at 09.15.02.png");
  assert.equal(inline.files[0]!.size, Buffer.from(png, "base64").length, "the picture's own bytes");
  const plain = await readDrop(snapshotDrop(transfer({ data: { "text/uri-list": "http://example.com/a.png" } })));
  assert.match(plain.problems[0] ?? "", /Only https links can be attached/);
  const blob = await readDrop(snapshotDrop(transfer({ data: { "text/uri-list": "blob:https://example.com/1234" } })));
  assert.match(blob.problems[0] ?? "", /neither a file nor an https link/);
  const nothing = await readDrop(snapshotDrop(transfer({ data: {} })));
  assert.match(nothing.problems[0] ?? "", /Nothing that can be sent was dropped/);
});

test("files win over the addresses a drag also carries (a picture dragged with its file)", async () => {
  const dropped = await readDrop(snapshotDrop(transfer({ items: [fileItem(file("cat.png", "x", "image/png"))], data: { "text/uri-list": "https://example.com/cat.png" } })));
  assert.deepEqual(dropped.files.map(item => item.name), ["cat.png"]);
  assert.deepEqual(dropped.links, []);
});

test("a browser without the entries API: the files as the drop lists them", async () => {
  const dropped = await readDrop(snapshotDrop({ files: [file("a.txt"), file("b.txt")], types: ["Files"], getData: () => "" }));
  assert.deepEqual(dropped.files.map(item => item.name), ["a.txt", "b.txt"]);
});

test("helpers: https links only, data pictures only, folder names in a sentence", () => {
  assert.equal(httpsLink(" https://example.com/x "), "https://example.com/x");
  for (const bad of ["http://example.com", "javascript:alert(1)", "https://user:pw@example.com", "https://exa mple.com", ""]) assert.equal(httpsLink(bad), null, bad);
  assert.equal(pictureFromDataUrl("data:text/html;base64,PHNjcmlwdD4="), null, "only pictures");
  assert.equal(pictureFromDataUrl("data:image/svg+xml;base64,PHN2Zz4="), null, "never an SVG (it could carry a script)");
  assert.deepEqual(addressesOf({ uriList: "", html: "", text: "https://example.com/a b" }), [], "text with spaces is not an address");
  assert.equal(foldersWords([{ name: "Photos" }]), "Photos");
  assert.equal(foldersWords([{ name: "Photos" }, { name: "Scans" }]), "Photos and Scans");
  assert.equal(foldersWords([{ name: "a" }, { name: "b" }, { name: "c" }]), "3 folders");
});

test("a folder of code: empty files (an empty __init__.py) and files the browser cannot read are left out and counted, the rest taken", async () => {
  const blank = (name: string): EntryLike => ({ isFile: true, isDirectory: false, name, file: success => setTimeout(() => success(new File([], name)), 0) });
  const broken = (name: string): EntryLike => ({ isFile: true, isDirectory: false, name, file: (_success, failure) => setTimeout(() => failure?.(new DOMException("gone", "NotFoundError")), 0) });
  const locked: EntryLike = { isFile: false, isDirectory: true, name: "secrets", createReader: () => ({ readEntries: (_success, failure) => failure?.(new DOMException("denied", "SecurityError")) }) };
  const project = folder("app", [entry("main.py"), blank("__init__.py"), folder("pkg", [blank("__init__.py"), entry("util.py")]), broken("link-to-nowhere"), locked]);
  const dropped = await readDrop(snapshotDrop(transfer({ items: [folderItem(project)] })));
  assert.deepEqual(dropped.files.map(item => item.name), ["main.py", "util.py"]);
  assert.deepEqual(dropped.folders, [{ name: "app", files: 2, hidden: 0, empty: 2, unreadable: 2, truncated: false }]);
  assert.deepEqual(dropped.problems, [], "the folder itself was read: what was left out is counted, not a failure");
  assert.equal(leftOutWords(dropped.folders), "2 empty files and 2 files this browser could not read were left out.");
  assert.equal(leftOutWords([{ empty: 1 }]), "One empty file was left out.");
  assert.equal(leftOutWords([{ unreadable: 1 }, { empty: 0 }]), "One file this browser could not read was left out.");
  assert.equal(leftOutWords([{ empty: 1200 }]), "1,200 empty files were left out.");
  assert.equal(leftOutWords([{}]), null);
});

test("files dragged from an app that gives a page only their addresses on disk (file://…) are said, never silently nothing", async () => {
  const dropped = await readDrop(snapshotDrop(transfer({ data: { "text/uri-list": "file:///Users/ada/Desktop/plan.pdf\nfile:///Users/ada/Desktop/notes.txt" } })));
  assert.deepEqual(dropped.files, []);
  assert.deepEqual(dropped.links, []);
  assert.match(dropped.problems[0] ?? "", /file:\/\/…\), which a web page cannot open/);
  assert.match(dropped.problems[0] ?? "", /\+\u00a0→\u00a0Upload a file/, "kept on one line: no-break spaces around the arrow");
});

const blankEntry = (name: string): EntryLike => ({ isFile: true, isDirectory: false, name, file: success => setTimeout(() => success(new File([], name)), 0) });
const scanEntries = (count: number) => Array.from({ length: count }, (_, index) => entry(`scan-${String(index + 1).padStart(3, "0")}.txt`));

test("a folder of more than 20 files dropped with a loose file: the loose one goes in at once, the question counts the folder's files only and names what was left out", async () => {
  const scans = folder("Scans", [...scanEntries(25), entry(".DS_Store"), blankEntry("empty.txt")], 10);
  const dropped = await readDrop(snapshotDrop(transfer({ items: [folderItem(scans), fileItem(file("cover.pdf", "%PDF", "application/pdf"))] })));
  assert.deepEqual(dropped.loose.map(item => item.name), ["cover.pdf"]);
  assert.equal(dropped.inFolders.length, 25);
  assert.equal(dropped.files.length, 26, "files is both, as before");
  const plan = planDrop(dropped, { room: 100, uploadsOff: null });
  assert.deepEqual(plan.add.map(item => item.name), ["cover.pdf"], "the loose file goes in at once, so Cancel never takes it back");
  assert.equal(plan.question?.words, "Scans has 25 files (one empty file left out). Upload all of them?");
  assert.equal(plan.question?.action, "Upload 25 files");
  assert.equal(plan.question?.files.length, 25);
  assert.deepEqual(plan.sayings, [], "what was left out is in the question, never a note hidden behind it");
});

test("loose files never make a folder ask: 3 files from a folder and 20 loose ones go in at once, and what was left out is said", async () => {
  const small = folder("Small", [entry("a.txt"), entry("b.txt"), blankEntry("c.txt")]);
  const loose = Array.from({ length: 20 }, (_, index) => fileItem(file(`loose-${index}.txt`)));
  const dropped = await readDrop(snapshotDrop(transfer({ items: [folderItem(small), ...loose] })));
  const plan = planDrop(dropped, { room: 100, uploadsOff: null });
  assert.equal(plan.question, null);
  assert.equal(plan.add.length, 22);
  assert.deepEqual(plan.sayings, ["One empty file was left out."]);
});

test("asked with less room than files: the first that fit; what else the drop says waits for the answer; no room at all is said", async () => {
  const locked: EntryLike = { isFile: false, isDirectory: true, name: "Locked", createReader: () => ({ readEntries: (_success, failure) => failure?.(new DOMException("denied", "SecurityError")) }) };
  const big = folder("Big", scanEntries(120), 50);
  const dropped = await readDrop(snapshotDrop(transfer({ items: [folderItem(big), folderItem(locked), ...Array.from({ length: 5 }, (_, index) => fileItem(file(`loose-${index}.txt`)))] })));
  const plan = planDrop(dropped, { room: 50, uploadsOff: null });
  assert.equal(plan.add.length, 5);
  assert.equal(plan.question?.words, "Big has 120 files, and a message carries up to 100 attachments. Upload the first 45?");
  assert.equal(plan.question?.files.length, 45);
  assert.match(plan.sayings[0] ?? "", /^The folder Locked could not be read/, "said once the question is answered (the composer keeps it for then)");
  const full = planDrop(dropped, { room: 5, uploadsOff: null });
  assert.equal(full.question, null);
  assert.ok(full.sayings.includes("This message already carries 100 attachments, the most it takes. Send it first."));
  const cut = await readDrop(snapshotDrop(transfer({ items: [folderItem(folder("Disk", scanEntries(40), 10))] })), { limit: 30 });
  assert.equal(planDrop(cut, { room: 100, uploadsOff: null }).question?.words, "Disk has more than 30 files. Upload all of them?");
});

test("uploads off: no file is added or asked about, links still go, and it says why once", async () => {
  const dropped = await readDrop(snapshotDrop(transfer({ items: [folderItem(folder("Scans", scanEntries(30), 10)), fileItem(file("a.txt"))] })));
  const plan = planDrop(dropped, { room: 100, uploadsOff: "Uploads are not set up on this DM website yet." });
  assert.deepEqual(plan.add, []);
  assert.equal(plan.question, null);
  assert.deepEqual(plan.sayings, ["Uploads are not set up on this DM website yet."]);
  const link = await readDrop(snapshotDrop(transfer({ data: { "text/uri-list": "https://example.com/notes" } })));
  assert.deepEqual(planDrop(link, { room: 100, uploadsOff: "Off." }), { links: ["https://example.com/notes"], add: [], question: null, sayings: [] });
});

test("left-out words inside a sentence", () => {
  assert.equal(leftOutWords([{ empty: 1 }], "inline"), "one empty file left out");
  assert.equal(leftOutWords([{ empty: 3, unreadable: 1 }], "inline"), "3 empty files and one file this browser could not read left out");
  assert.equal(leftOutWords([{}], "inline"), null);
});

