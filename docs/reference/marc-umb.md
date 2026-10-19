# MARC / MARCXML Reference

Source: https://www.loc.gov/marc/umb/

---

## Record Structure

Every MARC record has three layers:

**Leader** — 24 fixed-position characters at the start. Key positions:
- `[5]` Record status: `n`=new, `c`=corrected, `d`=deleted
- `[6]` Type of record: `a`=language material (books, articles), `e`=cartographic, `g`=projected medium, `j`=musical sound recording
- `[7]` Bibliographic level: `a`=article/component part, `m`=monograph/book, `s`=serial (journal run), `c`=collection
- `[6]=a + [7]=a` → journal article; `[6]=a + [7]=m` → book

**Control fields (00x)** — no indicators or subfields; value is a positional string:
- `001` — local record ID
- `008` — 40-char fixed data. Critical positions:
  - `[6]` Type of date: `s`=single, `m`=multiple, `r`=reprint, `t`=pub+copyright
  - `[7-10]` Date 1 — 4-digit publication year
  - `[11-14]` Date 2 — end year for ranges
  - `[15-17]` Place of publication (country code)
  - `[35-37]` Language code (e.g. `eng`)

**Variable fields** — tag (3 digits) + two single-char indicators + subfields. Each subfield = delimiter + single-char code + value. Fields and subfields are repeatable.

---

## Key Fields

### Identification

| Tag | Ind | Sub | Content |
|-----|-----|-----|---------|
| 001 | — | — | Local control number |
| 020 | `  ` | $a | ISBN-13 or ISBN-10; $z = canceled/invalid |
| 022 | `  ` | $a | ISSN (`NNNN-NNNN`); $l = linking ISSN |
| 024 | `7 ` | $a | **DOI value** (e.g. `10.1007/s123`); $2 = `doi`. Primary DOI field. |
| 035 | `  ` | $a | System control number (e.g. `(OCoLC)12345`) |

### Authors

| Tag | Ind | Sub | Content |
|-----|-----|-----|---------|
| 100 | `1 ` | $a | Main personal author: `Surname, Forename,`; $e = relator (`author.`) |
| 110 | `2 ` | $a | Corporate author |
| 700 | `1 ` | $a | Added personal author — repeat per co-author; same subfields as 100 |
| 710 | `2 ` | $a | Added corporate author |

### Title

| Tag | Ind | Sub | Content |
|-----|-----|-----|---------|
| 245 | `10` | $a | Title proper (strip trailing ` /` or ` :`) |
|     |     | $b | Subtitle |
|     |     | $c | Statement of responsibility |
|     |     | $n | Number of part |
|     |     | $p | Name of part |
| 246 | `  ` | $a | Varying/alternate title |

Ind2 of 245 = number of nonfiling characters (e.g. `4` for "The ").

### Publication Info

| Tag | Ind | Sub | Content |
|-----|-----|-----|---------|
| 260 | `  ` | $a | Place; $b = publisher; $c = date string (pre-RDA) |
| 264 | ` 1` | $a | Place; $b = publisher; $c = date string (RDA, preferred) |
| 264 | ` 4` | $c | Copyright date |

Date $c is free text: `2021.` or `©2021` or `[2021]`. **Always prefer `008[7:11]` for year.**

### Series / Journal Host

| Tag | Ind | Sub | Content |
|-----|-----|-----|---------|
| 490 | `0 ` | $a | Series title; $v = volume; $x = ISSN |
| 773 | `0 ` | $t | **Host journal title** (for articles); $g = parts string; $x = ISSN; $d = publisher |

`773 $g` is free text, e.g. `"v. 15, no. 3 (2021), p. 100-120"`. No standard format.

### Electronic Access

| Tag | Ind | Sub | Content |
|-----|-----|-----|---------|
| 856 | `40` | $u | **URL for full text** (may be `https://doi.org/10.xxx`) |
|     |     | $7 | Access: `0`=open, `1`=restricted (often absent — absence ≠ restricted) |
|     |     | $3 | Label (e.g. `Full text`) |
|     |     | $z | Public note |

Ind1=`4` = HTTP. Ind2=`0` = the resource itself; `1` = electronic version of print; `2` = related. Target ind2=`0` or `1` for full text.

---

## DOI Extraction (priority order)

1. `024` ind1=`7`, $2=`doi` → $a is the raw DOI
2. `856 $u` starting with `https://doi.org/` → strip prefix
3. `856 $u` starting with `http://dx.doi.org/` → strip prefix

## Year Extraction (priority order)

1. `008[7:11]` — always 4 digits (may contain `u` for unknown)
2. `264` ind2=`1` $c — first 4-digit sequence via regex
3. `260 $c` — same

---

## MARCXML Encoding

Namespace: `http://www.loc.gov/MARC21/slim`

```xml
<collection xmlns="http://www.loc.gov/MARC21/slim">
  <record>
    <leader>01234cam a2200000 a 4500</leader>
    <controlfield tag="001">ocn123456789</controlfield>
    <controlfield tag="008">210315s2021    nyu           000 0 eng d</controlfield>

    <datafield tag="024" ind1="7" ind2=" ">
      <subfield code="a">10.1007/978-3-030-22176-8</subfield>
      <subfield code="2">doi</subfield>
    </datafield>
    <datafield tag="100" ind1="1" ind2=" ">
      <subfield code="a">Smith, Jane,</subfield>
      <subfield code="e">author.</subfield>
    </datafield>
    <datafield tag="245" ind1="1" ind2="0">
      <subfield code="a">Deep learning :</subfield>
      <subfield code="b">a practical guide /</subfield>
    </datafield>
    <datafield tag="264" ind1=" " ind2="1">
      <subfield code="b">Springer,</subfield>
      <subfield code="c">2021.</subfield>
    </datafield>
    <datafield tag="700" ind1="1" ind2=" ">
      <subfield code="a">Jones, Bob,</subfield>
    </datafield>
    <datafield tag="856" ind1="4" ind2="0">
      <subfield code="u">https://doi.org/10.1007/978-3-030-22176-8</subfield>
      <subfield code="7">0</subfield>
    </datafield>
  </record>
</collection>
```

**Parsing rules:**
- `<leader>` → plain string, index by character position
- `<controlfield tag="008">` → plain string, slice `[7:11]` for year
- `<datafield tag="NNN" ind1="X" ind2="Y">` → match by `tag`; read `ind1`/`ind2` (blank = `" "`)
- `<subfield code="c">` → match child elements by `code`
- Fields repeat — collect all 700s, all 856s, all 020s

---

## Extraction Pseudocode

```python
title   = df("245").$a.rstrip(" /:") + " " + df("245").$b.rstrip(" /:")
authors = [df("100").$a] + [f.$a for f in all_df("700")]
year    = cf("008")[7:11]  # fallback: first 4-digit run in df("264",ind2="1").$c

isbns   = [f.$a for f in all_df("020")]
issns   = [f.$a for f in all_df("022")]

doi = next((f.$a for f in all_df("024") if f.ind1=="7" and f.$2=="doi"), None)
if not doi:
    doi = next((f.$u.removeprefix("https://doi.org/")
                for f in all_df("856") if "doi.org/" in (f.$u or "")), None)

urls = [(f.$u, f.$7=="0") for f in all_df("856")
        if f.ind1=="4" and f.ind2 in ("0","1") and f.$u]

is_article = leader[6]=="a" and leader[7]=="a"
is_book    = leader[6]=="a" and leader[7]=="m"
journal    = df("773").$t
parts      = df("773").$g  # free text: "v.5, no.2, p.100-120"
```

---

## Gotchas

- `245 $a/$b` often end with ISBD punctuation (` /`, ` :`, `,`) — strip before display
- `100 $a` is `Surname, Forename,` — trailing comma is MARC punctuation, not part of the name
- `008[7:11]` may contain `u` (unknown digit)
- `773 $g` has no standard format — use regex to extract year and page range
- `856 $7` is optional — absence does not mean open access
- Some records put DOI only in `856 $u`, not in `024`
- `260` (pre-RDA) and `264` (RDA) both appear in real dumps — check both
