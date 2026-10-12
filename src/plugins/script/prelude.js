/**
 * Runtime prelude for script plugins.
 *
 * Providers are written for React Native, so they reach for a few things that
 * are not part of the JavaScript language: `global`, `URLSearchParams`, and —
 * the big one — `cheerio`, which is how essentially every scraper reads HTML.
 * This supplies them, so a provider from the wild runs unmodified.
 *
 * The HTML parser here is deliberately forgiving rather than correct: scraped
 * pages are malformed constantly, and a parser that throws is useless. Unknown
 * markup is skipped, unclosed tags are closed for you, and a selector that
 * matches nothing yields an empty set instead of an error.
 */

// --------------------------------------------------------------- globals

var global = globalThis
globalThis.global = globalThis

if (typeof globalThis.SCRAPER_SETTINGS === 'undefined') {
  globalThis.SCRAPER_SETTINGS = {}
}

// --------------------------------------------------- URLSearchParams

function URLSearchParams(init) {
  this._pairs = []
  if (typeof init === 'string') {
    var text = init.charAt(0) === '?' ? init.slice(1) : init
    var parts = text.split('&')
    for (var i = 0; i < parts.length; i++) {
      if (!parts[i]) continue
      var eq = parts[i].indexOf('=')
      if (eq === -1) this._pairs.push([decodeURIComponent(parts[i]), ''])
      else
        this._pairs.push([
          decodeURIComponent(parts[i].slice(0, eq)),
          decodeURIComponent(parts[i].slice(eq + 1).replace(/\+/g, ' ')),
        ])
    }
  } else if (init && typeof init === 'object') {
    for (var key in init) {
      if (Object.prototype.hasOwnProperty.call(init, key)) {
        this._pairs.push([key, String(init[key])])
      }
    }
  }
}

URLSearchParams.prototype.append = function (name, value) {
  this._pairs.push([String(name), String(value)])
}
URLSearchParams.prototype.set = function (name, value) {
  var found = false
  var out = []
  for (var i = 0; i < this._pairs.length; i++) {
    if (this._pairs[i][0] === name) {
      if (found) continue
      out.push([String(name), String(value)])
      found = true
    } else out.push(this._pairs[i])
  }
  if (!found) out.push([String(name), String(value)])
  this._pairs = out
}
URLSearchParams.prototype.get = function (name) {
  for (var i = 0; i < this._pairs.length; i++) {
    if (this._pairs[i][0] === name) return this._pairs[i][1]
  }
  return null
}
URLSearchParams.prototype.has = function (name) {
  return this.get(name) !== null
}
URLSearchParams.prototype.toString = function () {
  var out = []
  for (var i = 0; i < this._pairs.length; i++) {
    out.push(
      encodeURIComponent(this._pairs[i][0]) + '=' + encodeURIComponent(this._pairs[i][1]),
    )
  }
  return out.join('&')
}
globalThis.URLSearchParams = URLSearchParams

// ------------------------------------------------------------ HTML parse

var VOID_TAGS = {
  area: 1, base: 1, br: 1, col: 1, embed: 1, hr: 1, img: 1, input: 1,
  link: 1, meta: 1, param: 1, source: 1, track: 1, wbr: 1,
}

/** Tag attributes, tolerating unquoted and valueless forms. */
function parseAttrs(raw) {
  var attrs = {}
  var re = /([a-zA-Z_:][-a-zA-Z0-9_:.]*)\s*(?:=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+)))?/g
  var m
  while ((m = re.exec(raw)) !== null) {
    var value = m[2] !== undefined ? m[2] : m[3] !== undefined ? m[3] : m[4] !== undefined ? m[4] : ''
    attrs[m[1].toLowerCase()] = value
  }
  return attrs
}

function decodeEntities(text) {
  return text
    .replace(/&nbsp;/g, ' ')
    .replace(/&amp;/g, '&')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&#0?39;/g, "'")
    .replace(/&#x27;/gi, "'")
    .replace(/&#(\d+);/g, function (_, code) {
      return String.fromCharCode(parseInt(code, 10))
    })
}

/**
 * Builds a tree of { tag, attrs, children, parent, text }.
 *
 * script and style contents are swallowed whole so their contents cannot be
 * mistaken for markup.
 */
function parseHTML(html) {
  var root = { tag: '#root', attrs: {}, children: [], parent: null }
  var stack = [root]
  var i = 0

  while (i < html.length) {
    var lt = html.indexOf('<', i)
    if (lt === -1) {
      addText(stack[stack.length - 1], html.slice(i))
      break
    }
    if (lt > i) addText(stack[stack.length - 1], html.slice(i, lt))

    // comment / doctype
    if (html.slice(lt, lt + 4) === '<!--') {
      var close = html.indexOf('-->', lt)
      i = close === -1 ? html.length : close + 3
      continue
    }
    if (html.charAt(lt + 1) === '!') {
      var end = html.indexOf('>', lt)
      i = end === -1 ? html.length : end + 1
      continue
    }

    var gt = html.indexOf('>', lt)
    if (gt === -1) break
    var inner = html.slice(lt + 1, gt)
    i = gt + 1

    if (inner.charAt(0) === '/') {
      var closing = inner.slice(1).trim().toLowerCase()
      for (var s = stack.length - 1; s > 0; s--) {
        if (stack[s].tag === closing) {
          stack.length = s
          break
        }
      }
      continue
    }

    var selfClosing = inner.charAt(inner.length - 1) === '/'
    if (selfClosing) inner = inner.slice(0, -1)

    var spaceMatch = /\s/.exec(inner)
    var space = spaceMatch ? spaceMatch.index : -1
    var tag = (space === -1 ? inner : inner.slice(0, space)).toLowerCase()
    var attrs = space === -1 ? {} : parseAttrs(inner.slice(space))

    var node = { tag: tag, attrs: attrs, children: [], parent: stack[stack.length - 1] }
    stack[stack.length - 1].children.push(node)

    if (tag === 'script' || tag === 'style') {
      var closeTag = '</' + tag
      var bodyEnd = html.toLowerCase().indexOf(closeTag, i)
      if (bodyEnd === -1) bodyEnd = html.length
      addText(node, html.slice(i, bodyEnd))
      var after = html.indexOf('>', bodyEnd)
      i = after === -1 ? html.length : after + 1
      continue
    }

    if (!selfClosing && !VOID_TAGS[tag]) stack.push(node)
  }
  return root
}

function addText(parent, text) {
  if (!text) return
  parent.children.push({ tag: '#text', text: text, attrs: {}, children: [], parent: parent })
}

// --------------------------------------------------------------- selectors

/** One compound selector: `div#id.cls[attr=val]`. */
function parseSimple(token) {
  var part = { tag: null, id: null, classes: [], attrs: [] }
  var re = /(^[a-zA-Z*][-a-zA-Z0-9]*)|#([-\w]+)|\.([-\w]+)|\[([^\]]+)\]/g
  var m
  while ((m = re.exec(token)) !== null) {
    if (m[1]) part.tag = m[1].toLowerCase()
    else if (m[2]) part.id = m[2]
    else if (m[3]) part.classes.push(m[3])
    else if (m[4]) {
      var attr = m[4]
      var eq = attr.indexOf('=')
      if (eq === -1) part.attrs.push({ name: attr.trim(), value: null, op: null })
      else {
        var name = attr.slice(0, eq).trim()
        var op = null
        if (/[\^$*~|]$/.test(name)) {
          op = name.charAt(name.length - 1)
          name = name.slice(0, -1)
        }
        var value = attr.slice(eq + 1).trim().replace(/^["']|["']$/g, '')
        part.attrs.push({ name: name, value: value, op: op })
      }
    }
  }
  return part
}

function matchesSimple(node, part) {
  if (node.tag === '#text' || node.tag === '#root') return false
  if (part.tag && part.tag !== '*' && node.tag !== part.tag) return false
  if (part.id && node.attrs.id !== part.id) return false
  for (var c = 0; c < part.classes.length; c++) {
    var cls = (node.attrs['class'] || '').split(/\s+/)
    if (cls.indexOf(part.classes[c]) === -1) return false
  }
  for (var a = 0; a < part.attrs.length; a++) {
    var want = part.attrs[a]
    var have = node.attrs[want.name.toLowerCase()]
    if (have === undefined) return false
    if (want.value === null) continue
    if (want.op === '^' && have.indexOf(want.value) !== 0) return false
    else if (want.op === '$' && have.slice(-want.value.length) !== want.value) return false
    else if (want.op === '*' && have.indexOf(want.value) === -1) return false
    else if (!want.op && have !== want.value) return false
  }
  return true
}

/** Descendant combinators only — enough for what scrapers write. */
function select(roots, selector) {
  var results = []
  var groups = selector.split(',')

  for (var g = 0; g < groups.length; g++) {
    var parts = groups[g].trim().split(/\s+/).filter(Boolean).map(parseSimple)
    if (!parts.length) continue

    var current = roots.slice()
    for (var p = 0; p < parts.length; p++) {
      var next = []
      for (var r = 0; r < current.length; r++) {
        collectDescendants(current[r], parts[p], next)
      }
      current = next
    }
    for (var k = 0; k < current.length; k++) {
      if (results.indexOf(current[k]) === -1) results.push(current[k])
    }
  }
  return results
}

function collectDescendants(node, part, out) {
  for (var i = 0; i < node.children.length; i++) {
    var child = node.children[i]
    if (matchesSimple(child, part) && out.indexOf(child) === -1) out.push(child)
    collectDescendants(child, part, out)
  }
}

function textOf(node) {
  if (node.tag === '#text') return node.text
  var out = ''
  for (var i = 0; i < node.children.length; i++) out += textOf(node.children[i])
  return out
}

function elementChildren(node) {
  var out = []
  for (var i = 0; i < node.children.length; i++) {
    if (node.children[i].tag !== '#text') out.push(node.children[i])
  }
  return out
}

// ------------------------------------------------------------------ cheerio

function makeCheerio(html) {
  var root = parseHTML(html)

  function Selection(nodes) {
    this._nodes = nodes || []
    this.length = this._nodes.length
    for (var i = 0; i < this._nodes.length; i++) this[i] = this._nodes[i]
  }

  Selection.prototype.find = function (selector) {
    return new Selection(select(this._nodes, selector))
  }
  Selection.prototype.attr = function (name) {
    if (!this._nodes.length) return undefined
    return this._nodes[0].attrs[String(name).toLowerCase()]
  }
  Selection.prototype.text = function () {
    var out = ''
    for (var i = 0; i < this._nodes.length; i++) out += textOf(this._nodes[i])
    return decodeEntities(out)
  }
  Selection.prototype.html = function () {
    if (!this._nodes.length) return null
    return textOf(this._nodes[0])
  }
  Selection.prototype.each = function (fn) {
    for (var i = 0; i < this._nodes.length; i++) {
      fn.call(this._nodes[i], i, this._nodes[i])
    }
    return this
  }
  Selection.prototype.map = function (fn) {
    var out = []
    for (var i = 0; i < this._nodes.length; i++) {
      out.push(fn.call(this._nodes[i], i, this._nodes[i]))
    }
    return new Selection(out)
  }
  Selection.prototype.get = function (index) {
    if (index === undefined) return this._nodes.slice()
    return this._nodes[index < 0 ? this._nodes.length + index : index]
  }
  Selection.prototype.toArray = function () {
    return this._nodes.slice()
  }
  Selection.prototype.eq = function (index) {
    var node = this._nodes[index < 0 ? this._nodes.length + index : index]
    return new Selection(node ? [node] : [])
  }
  Selection.prototype.first = function () {
    return this.eq(0)
  }
  Selection.prototype.last = function () {
    return this.eq(this._nodes.length - 1)
  }
  Selection.prototype.children = function (selector) {
    var out = []
    for (var i = 0; i < this._nodes.length; i++) {
      var kids = elementChildren(this._nodes[i])
      for (var k = 0; k < kids.length; k++) out.push(kids[k])
    }
    var sel = new Selection(out)
    return selector ? sel.filter(selector) : sel
  }
  Selection.prototype.parent = function () {
    var out = []
    for (var i = 0; i < this._nodes.length; i++) {
      var p = this._nodes[i].parent
      if (p && p.tag !== '#root' && out.indexOf(p) === -1) out.push(p)
    }
    return new Selection(out)
  }
  Selection.prototype.next = function () {
    var out = []
    for (var i = 0; i < this._nodes.length; i++) {
      var node = this._nodes[i]
      if (!node.parent) continue
      var sibs = elementChildren(node.parent)
      var at = sibs.indexOf(node)
      if (at !== -1 && sibs[at + 1]) out.push(sibs[at + 1])
    }
    return new Selection(out)
  }
  Selection.prototype.prev = function () {
    var out = []
    for (var i = 0; i < this._nodes.length; i++) {
      var node = this._nodes[i]
      if (!node.parent) continue
      var sibs = elementChildren(node.parent)
      var at = sibs.indexOf(node)
      if (at > 0) out.push(sibs[at - 1])
    }
    return new Selection(out)
  }
  Selection.prototype.filter = function (selector) {
    var part = parseSimple(String(selector).trim())
    var out = []
    for (var i = 0; i < this._nodes.length; i++) {
      if (matchesSimple(this._nodes[i], part)) out.push(this._nodes[i])
    }
    return new Selection(out)
  }
  Selection.prototype.is = function (selector) {
    return this.filter(selector).length > 0
  }

  /** `$(selector)`, `$(node)`, `$(selection)`. */
  function $(what) {
    if (what === undefined || what === null) return new Selection([])
    if (what instanceof Selection) return what
    if (typeof what === 'object') return new Selection([what])
    return new Selection(select([root], String(what)))
  }

  $.root = function () {
    return new Selection([root])
  }
  $.html = function (sel) {
    return sel && sel.html ? sel.html() : ''
  }
  $.text = function (sel) {
    return sel && sel.text ? sel.text() : ''
  }
  return $
}

var cheerio = {
  load: function (html) {
    return makeCheerio(String(html === undefined || html === null ? '' : html))
  },
}
globalThis.cheerio = cheerio

// ------------------------------------------------------------------ require

/**
 * Just enough module resolution for providers that were bundled for Node.
 * Anything unknown returns an empty object rather than throwing, so a stray
 * import cannot take down a provider that never uses it.
 */
function require(name) {
  if (String(name).indexOf('cheerio') !== -1) return cheerio
  if (String(name) === 'url') return { URLSearchParams: URLSearchParams }
  return {}
}
globalThis.require = require
