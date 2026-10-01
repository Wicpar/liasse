# Writing Rules

These rules govern SPEC.md, LANGUAGE.md, BUILTINS.md, ALPHA.md, POSTGRES.md, and
CONNECT.md. Each rule here is a general rule of writing, and holds for every
feature that the documents describe. When the owner corrects the writing, the
general principle behind the correction goes here, in the same pass as the fix,
and its specifics stay in the documents.

## 1. One document for each sentence

| Document | Holds |
| --- | --- |
| LANGUAGE.md | the syntax, and what each form means |
| BUILTINS.md | each builtin: its signature and one line |
| SPEC.md | the behavior of the Liasse system, whatever implements it |
| ALPHA.md | the first implementation: crates, Rust API, host implementations, tests |
| POSTGRES.md | the PostgreSQL backend: schema, statements, obligations, gates |
| CONNECT.md | the alpha's client: its binding, its operations and messages, and the web client |

- A sentence that changes when a builtin changes belongs where the builtin is
  specified: in SPEC for the builtins through which modules interact, and in
  BUILTINS for every other. A sentence about what the running system does
  belongs in SPEC. A sentence about one implementation belongs in its component
  document.
- A feature that one implementation provides, and another may leave out,
  belongs in that implementation's document.
- Less is better in the core documents: what an implementation's document can
  hold leaves SPEC, LANGUAGE, and BUILTINS.
- LANGUAGE shows builtins in examples, and names one in prose only to explain
  the form that the example shows. It cites no section of another document.
- SPEC holds general behavior and guarantees: what holds for every module,
  call, and value. What one module does, such as which values a mutation
  stores, is an example at most. SPEC names no store, binding, encoding, crate,
  index, or query plan, and it recommends nothing. Its keywords are MUST,
  MUST NOT, and MAY (BCP 14). Its examples MAY use every builtin. Its prose
  names the builtins that it specifies, and the concepts that every other
  builtin, types included, declares; BUILTINS gives the signature of each.
- SPEC states the capabilities and the guarantees that a party can rely on, and
  never the internals that provide them. What a consumer needs is the
  implementation's to provide, and a component that serves a party, such as a
  client library, belongs to the implementation's document.

## 2. Say what things are and do

- Define a thing by what it is, holds, and does. "Calls zero effects", "reads
  its parameters alone", "fails to build", "stays as it was", and "form an
  acyclic graph" say it. "Calls no effect", "does not build", "has no",
  "nothing", "without", "instead of", "rather than", "unlike", and "other than"
  define by exclusion.
- A negation stays where it is itself the content, and where it excludes what
  the positive statements beside it leave open:
  - a prohibition: a MUST NOT, or its plain form in a component document;
  - the condition of a rejection, a failure, a warning, a selection, or a
    branch: "when none matches";
  - a mathematical definition or validity condition: "no two rows hold equal
    values";
  - a guarantee that rules out a failure or a wait: "never deadlocks";
  - a count of zero in a test or a gate;
  - absence as a thing that the system represents or reads;
  - in a component document, the reason for a design choice, and a prior-art
    pattern that the design declines, with its reason;
  - a quotation or a title, as written.
- A negation that the sentence reads the same without goes, whatever its
  category: one that excludes an option that no sentence offers, and one that
  repeats the positive statement beside it.
- A negation with a positive counterpart takes it, in these cases too: "fails
  to parse", "differs from", "is still ahead", "alone".
- A secret is stated by where it stays, and by the channels that its copies
  reach none of.
- A sentence that gives a party access names what authorizes it: no document
  grants anything to every client by default.
- The documents describe the system as it is. They hold no history, no
  changelog, and no mention of a removed or deferred feature. Git is the
  history.

## 3. Rules, not their consequences

- State each rule once, where it belongs. A consequence of the stated rules gets
  no term, no section, and no rule of its own.
- An example MAY show a consequence, when the reader gains from seeing it.
- A general rule covers its cases, and a case restates none of it.
- Before adding a sentence, search the documents for its key nouns. A fact
  already stated is named by its term, and its content is left where it is.

## 4. Terms

- One term names one thing within its context, and one thing has one term. The
  term is bold in the sentence that defines it. Another context MAY use the same
  word for its own thing.
- A term names what its thing is: its head noun names the thing's kind, and a
  thing of another kind takes a term with another head.
- A case of a defined thing takes that thing's term, and gets no term of its
  own.
- A generic word names things of one kind, and each use says which one it
  names.
- A concept keeps its term, distinct from the name of the builtin that
  implements it.
- A term is defined before its first use. A document goes from low to high
  level, and each section uses only forms introduced before it.

## 5. Sentences and examples

- Subject, verb, object. Each sentence and each clause carries meaning: a
  sentence that announces, frames, or restates the one beside it goes.
- Show, don't tell. An example replaces the prose that it makes obvious, and
  prose explains what an example cannot show.
- Examples are valid code: they parse, each call stands where the documents
  allow it, and each name that they use is declared.
- Lists hold parallel items, tables hold comparisons, and numbered steps hold
  procedures.
- A cross-reference points to content that the reader needs at that place. It
  never attributes a fact that the sentence already states.
- Each document keeps its wrapping: prose at 80 columns, and one line for each
  bullet or table row where the document already does that.
- No emojis.

## 6. Reviewing

- A correction from the owner states a principle. Apply it everywhere it holds,
  and record its general rule here.
- Verify by reading. Grep finds candidates, and reading decides. The documents
  are sources of truth, and no script tests them.
- Every new or changed sentence passes each rule here before it is reported:
  its document, a positive form, a rule rather than a case or a consequence,
  one statement of each fact, and its terms. The request that motivates a
  sentence is its reason, and its wording stays in the request.
- A correction is a principle, and a grep for its wording finds only its
  lexical cases. The documents are read in full against every rule here.
- Syntax that the owner has not approved stays out.
