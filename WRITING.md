# Writing Rules

These rules govern SPEC.md, LANGUAGE.md, BUILTINS.md, ALPHA.md, POSTGRES.md, and
CONNECT.md. Each rule here is a general rule of writing, and holds for every
feature that the documents describe. When the owner corrects the writing, the
general principle behind the correction goes here, in the same pass as the fix,
and its specifics stay in the documents.

Knuth, Larrabee, and Roberts's
[Mathematical Writing](https://www-cs-faculty.stanford.edu/~knuth/papers/cs1193.pdf)
is the mandatory guide. Apply its principles to the reader's understanding:
motivation, order of presentation, explanations, examples, and notation.

## 1. One document for each sentence

| Document | Holds |
| --- | --- |
| LANGUAGE.md | the syntax, and what each form means |
| BUILTINS.md | core library signatures and their observable contracts |
| SPEC.md | the portable archive format and system guarantees beyond the language |
| ALPHA.md | the first implementation: crates, Rust API, host implementations, tests |
| POSTGRES.md | the PostgreSQL backend: schema, statements, obligations, gates |
| CONNECT.md | the alpha's client: its binding, its operations and messages, and the web client |

- A library-specific rule belongs beside its declaration in BUILTINS. A rule
  about source syntax and meaning belongs in LANGUAGE. SPEC fills the remaining
  gaps in system behavior and import/export. Implementation policy belongs in
  the component document.
- A feature that one implementation provides, and another may leave out,
  belongs in that implementation's document.
- Less is better in the core documents: what an implementation's document can
  hold leaves SPEC, LANGUAGE, and BUILTINS.
- LANGUAGE shows builtins in examples, and names one in prose only to explain
  the form that the example shows. It cites no section of another document.
- SPEC defines the archive layout, its portable data encoding, and observable
  guarantees that implementations share. Keep backend storage, scheduling,
  client protocols, and library implementation choices in component documents.
  Its keywords are MUST, MUST NOT, and MAY (BCP 14).
- Define only what interoperability requires. Reuse the language's meanings and
  the existing data structures; add a representation rule only when readers
  would otherwise disagree. Portability may require adapters for host libraries.
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

## 3. Rules and explanations

- Each rule has one authoritative statement, in the document responsible for
  it. Complementary explanations and examples MAY revisit it when they give
  the reader another way to understand it.
- A section develops one coherent subject. A detail belongs with the concept
  it explains; its length alone gives it no independent subject.
- Keep logical ownership distinct from packaging and storage. Describe data
  with its owner and preserve that boundary in formats and contracts.
- A portable artifact carries the content its data references. Check that it
  remains complete at every boundary advertised as independently usable.
- For bulk data, define columns once and show values as rows. State the
  column order and carry explicit headers in the encoded tables, including
  empty tables.
- Use named fields for structure and operations, and tables for bulk data.
  Check a complete nested example: readers should see what each part means
  without counting positions or looking up a separate layout.
- Give editable information one authoritative representation. Show what the
  reader can change directly and how that change affects the result.
- Explain a consequence when it helps the reader apply a rule. Make clear how
  it follows, and keep the general rule authoritative for all its cases.
- Before adding a sentence, search the documents for its key nouns. A fact
  already stated keeps its term and authoritative home. Further explanation
  must add understanding.

## 4. Terms

- One term names one thing within its context, and one thing has one term. The
  term is bold in the sentence that defines it. Another context MAY use the same
  word for its own thing.
- A term names what its thing is: its head noun names the thing's kind, and a
  thing of another kind takes a term with another head.
- A case of a defined thing uses that thing's term. A narrower term needs a
  distinction that matters to the reader.
- A function's name says the types that it is for, and a conversion's name says
  both of its types.
- A generic word names things of one kind, and each use says which one it
  names.
- A concept keeps its term, distinct from the name of the builtin that
  implements it.
- Explain each term and symbol at its first use, at least informally.
  Introduce notation with the concept it expresses, where the reader first
  needs it. An introductory example MAY precede the full definitions when its
  explanation gives the reader enough to understand it.

## 5. Sentences and examples

- Subject, verb, object. Each sentence carries an assertion, a motivation, or
  a connection the reader needs. Cut empty announcements and repetition that
  adds no understanding.
- Give the reader a purpose and a concrete starting point. Develop simple
  cases before abstractions and combinations. Assume ordinary programming
  knowledge; explain what the language adds or changes.
- Aim for the shortest explanation that gives full understanding. A length
  target is no reason to retain text. Keep a detail only when it helps the
  reader understand or apply the concept.
- Prose and examples work together. Explain what an example demonstrates and
  how the rule gives its result. A complex rule needs a worked example that
  shows its significant steps.
- Prose and examples define the language for its readers. Keep the complete
  parsing grammar in an annex; grammar notation cannot replace an explanation.
  The prose carries the argument when the reader skims formulas or code.
  Transitions explain how the parts connect.
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
  its document, precise wording, its role as a rule, explanation, or
  example, and its terms. Check what the reader knows at that point and how
  the sentence advances their understanding. The request that motivates a
  sentence is its reason, and its wording stays in the request.
- A correction is a principle, and a grep for its wording finds only its
  lexical cases. The documents are read in full against every rule here.
- Syntax that the owner has not approved stays out.
