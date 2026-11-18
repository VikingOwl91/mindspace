#set document(title: "Northglass Field Guide", author: "Mindspace contributors")
#set page(paper: "a5", margin: 16mm)
#set text(font: "Libertinus Serif", size: 11pt)
#let column_pair(l, r) = grid(columns: (1fr, 1fr), column-gutter: 14pt, l, r)
#let name(value) = block(text(size: 15pt, weight: "bold", value))
#let fact(label, value) = block([*#label:* #value])

#text(size: 20pt, weight: "bold")[Northglass Field Guide]
#block[Edition 2]
#v(12pt)
#column_pair([
  #name[ Rime Hart ]
  #fact[Habitat][Lichen ridges]
  #fact[Damage Resistances][cold]
  #fact[Actions][Antler sweep deals bludgeoning damage.]
], [
  #name[ Lantern Eel ]
  #fact[Habitat][Flooded caves]
  #fact[Appearance][Amber spots ring its gills.]
])

#pagebreak()
#column_pair([
  #fact[Damage Resistances][cold only while submerged]
  #fact[Actions][Tail whip deals bludgeoning damage.]
], [
  #name[ Glass Jackal ]
  #fact[Habitat][Salt flats]
  #fact[Damage Resistances][fire]
  #fact[Actions][Glacial bite deals cold damage.]
])

#pagebreak()
#image("plate.png", width: 100%, alt: none)
