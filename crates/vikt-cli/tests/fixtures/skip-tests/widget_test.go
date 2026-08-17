// Table-driven suite for BuildWidget — the file a default folder walk must
// skip, matching Go's `*_test.go` registry convention.
package widget

import "testing"

func TestBuildWidget(t *testing.T) {
	cases := []struct {
		count int
		name  string
		want  string
	}{
		{0, "gear", "no gear"},
		{1, "gear", "1 gear"},
		{3, "gear", "3 gears"},
		{12, "gear", "12 gears"},
	}
	for _, c := range cases {
		if got := BuildWidget(c.count, c.name); got != c.want {
			t.Errorf("BuildWidget(%d, %q) = %q, want %q", c.count, c.name, got, c.want)
		}
	}
}
