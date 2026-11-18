"""Synthetic defense-line controls, independent of any book or creature name."""
from unittest import TestCase

from .bestiary_fields import is_resistance_field, resistance_value


class ResistanceFieldsTests(TestCase):
    def test_numeric_value_belongs_to_its_type(self):
        field = "HP 84; Immunities sleep; Weaknesses radiant 4; Resistances Acid 6, Cold 12"
        self.assertTrue(is_resistance_field(field))
        self.assertEqual(resistance_value(field, "cold"), "Cold 12")
        self.assertEqual(resistance_value(field, "acid"), "Acid 6")
        for target in ("sleep", "radiant", "fire"):
            self.assertIsNone(resistance_value(field, target))

    def test_preserves_condition_and_numeric_amount(self):
        for line, expected in (
            ("Damage Resistances: cold only while submerged", "cold only while submerged"),
            ("Resistances cold 10 (except at dawn)", "cold 10 (except at dawn)"),
            ("Resistances acid 3, cold 4 until the ice melts", "cold 4 until the ice melts"),
            ("Armor: plated scales; resistant to cold only while dormant.", "resistant to cold only while dormant"),
            ("Armour: Resistant to cold.", "Resistant to cold"),
        ):
            with self.subTest(line=line):
                self.assertTrue(is_resistance_field(line))
                self.assertEqual(resistance_value(line, "cold"), expected)

    def test_excludes_neighboring_armor_fields(self):
        self.assertIsNone(resistance_value("Armor: armored shell; Spells: resistant to cold.", "cold"))
        self.assertIsNone(resistance_value("Armor: none; immune to cold.", "cold"))
        self.assertIsNone(resistance_value("Armor: none; not resistant to cold.", "cold"))
        self.assertIsNone(resistance_value("Armor: none; makes the target resistant to cold.", "cold"))

    def test_rejects_mentions_inside_conditions_and_compound_types(self):
        for line in (
            "Resistances fire only when exposed to wind and cold",
            "Resistances physical 8 (except cold iron)",
            "Resistances cold iron 4",
            "Resistances: cold not resisted",
            "Resistances: cold (immune)",
            "Resistance: cold (weakness)",
            "Resistances fire, but not cold",
            "Resistances to all except cold",
            "Immunities or Resistances cold",
            "Weaknesses cold 3; Resistances fire 5",
            "Damage Immunities: cold",
            "Actions: deals cold damage; Resistances cold 10",
            "Those who have Resistances cold 3 are described below.",
        ):
            with self.subTest(line=line):
                self.assertIsNone(resistance_value(line, "cold"))

    def test_semicolons_between_damage_types_keep_legacy_conditions(self):
        self.assertEqual(resistance_value("Resistances: fire; cold only at night", "cold"),
                         "cold only at night")
        self.assertIsNone(resistance_value("Resistances: fire; Weaknesses: cold", "cold"))

    def test_grouped_armor_values_preserve_source_context(self):
        line = "Armor: scales; resistant to fire and cold while submerged."
        self.assertEqual(resistance_value(line, "cold"), "resistant to fire and cold while submerged")
        self.assertIsNone(resistance_value("Armor: scales; resistant to fire but vulnerable to cold.", "cold"))
