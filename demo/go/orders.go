// The canonical cross-language demo, in Go. Compare demo/rust, demo/python,
// demo/java and demo/kotlin: same program, same expected tiering. Also
// exercises Go-specific shapes the other demos don't need: a pointer-
// receiver method (Type::method naming), `:=`/var/const bindings, a
// range-for loop, a switch, a select, a defer and a launched goroutine.
package demo

import "fmt"

var runningTotal float64

// Counter demonstrates method receiver naming (Counter::Add) and a struct
// field write through a pointer receiver.
type Counter struct {
	total float64
}

// Add is a pointer-receiver method.
func (c *Counter) Add(amount float64) float64 {
	c.total = c.total + amount
	return c.total
}

func Process(prices []float64, taxRate float64, applyTax bool) float64 {
	fmt.Println("processing", len(prices), "prices")
	unused := "this value goes nowhere"
	inspected := 0
	subtotal := 0.0
	for _, price := range prices {
		if price < 0 {
			continue
		}
		subtotal += price
		inspected++
	}
	total := subtotal
	if applyTax {
		total = subtotal * (1.0 + taxRate)
	}
	fmt.Println("inspected", inspected, "entries")
	_ = unused
	runningTotal = total
	return total
}

// Category demonstrates a value switch over a discriminant.
func Category(total float64) string {
	switch {
	case total > 1000:
		return "large"
	case total > 100:
		return "medium"
	default:
		return "small"
	}
}

func worker(total float64) {
	fmt.Println("worker saw", total)
}

func cleanup() {
	fmt.Println("cleanup")
}

// Dispatch demonstrates defer, a launched goroutine and panic-as-a-call.
func Dispatch(total float64) {
	defer cleanup()
	go worker(total)
	if total < 0 {
		panic("negative total")
	}
}

// Await demonstrates select over two channels.
func Await(done chan bool, results chan float64) float64 {
	select {
	case v := <-results:
		return v
	case <-done:
		return 0
	}
}
