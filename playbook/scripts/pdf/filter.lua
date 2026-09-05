-- Pandoc filter for the spec PDF. The HTML comes from the prerendered
-- site, so this maps the site's small vocabulary onto LaTeX.

-- The pages break lines after each sentence. Paper prose flows.
function LineBreak()
  return pandoc.Space()
end

-- <mark> is the page's skim layer. In print, bold carries it.
function Span(el)
  if el.classes:includes("mark") then
    return pandoc.Strong(el.content)
  end
  if el.classes:includes("tag-stretch") then
    return pandoc.SmallCaps({ pandoc.Str("[") } .. el.content .. { pandoc.Str("]") })
  end
  return el.content
end

function Link(el)
  -- Page-relative links (./03, ./00#terms) point nowhere in a PDF.
  if el.target:match("^%./") or el.target:match("^#") then
    return el.content
  end
  return el
end

-- rsvg gives the figures their own PDF; scale them to the text width.
function Image(el)
  el.attributes.width = "100%"
  return el
end

-- Keep the site's section numbering: page 00 is section 0.
function Pandoc(doc)
  local blocks = doc.blocks
  table.insert(blocks, 1, pandoc.RawBlock("latex", "\\setcounter{section}{-1}"))
  return doc
end

-- The site's tables set no column widths, so pandoc emits natural-width
-- columns that run off the page. Weight each column by its longest cell,
-- capped so one verbose column cannot starve the others.
function Table(el)
  local n = #el.colspecs
  local weights = {}
  for i = 1, n do weights[i] = 4 end
  local function scan(rows)
    for _, row in ipairs(rows) do
      for i, cell in ipairs(row.cells) do
        local len = #pandoc.utils.stringify(cell.contents)
        if len > weights[i] then weights[i] = math.min(len, 60) end
      end
    end
  end
  scan(el.head.rows)
  for _, body in ipairs(el.bodies) do scan(body.body) end
  local total = 0
  for i = 1, n do total = total + weights[i] end
  for i = 1, n do
    el.colspecs[i] = { el.colspecs[i][1], weights[i] / total }
  end
  return el
end
