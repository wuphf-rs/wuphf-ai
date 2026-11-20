package instagramconnector

import (
	"fmt"
	"math/big"
	"strings"
)

// shortcodeAlphabet is the URL-safe base64 alphabet Instagram encodes media ids
// with: a shortcode is the id in base 64, big-endian, 6 bits per character.
const shortcodeAlphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"

// publicShortcodeLength is the length of a public post's shortcode. A private
// account's links carry a longer code whose first characters are the id.
const publicShortcodeLength = 11

// MediaIDFromShortcode decodes a post or reel shortcode to the numeric media id
// the media API takes.
func MediaIDFromShortcode(code string) (string, error) {
	if code == "" {
		return "", fmt.Errorf("empty shortcode")
	}
	if len(code) > publicShortcodeLength {
		code = code[:publicShortcodeLength]
	}
	id := new(big.Int)
	for _, character := range code {
		digit := strings.IndexRune(shortcodeAlphabet, character)
		if digit < 0 {
			return "", fmt.Errorf("shortcode %q has a character outside the alphabet: %q", code, character)
		}
		id.Lsh(id, 6)
		id.Or(id, big.NewInt(int64(digit)))
	}
	return id.String(), nil
}
