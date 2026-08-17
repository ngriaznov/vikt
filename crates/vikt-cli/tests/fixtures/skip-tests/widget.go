// Fixture production source for the folder-walk test-skip integration test.
package widget

// BuildWidget assembles a label from a count and a name, pluralizing past
// one — enough real branching that it survives function-body validation.
func BuildWidget(count int, name string) string {
	if count <= 0 {
		return "no " + name
	}
	if count == 1 {
		return "1 " + name
	}
	return itoa(count) + " " + name + "s"
}

func itoa(n int) string {
	if n == 0 {
		return "0"
	}
	digits := ""
	for n > 0 {
		d := n % 10
		digits = string(rune('0'+d)) + digits
		n /= 10
	}
	return digits
}
