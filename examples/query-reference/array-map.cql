#repo = "example_arrays" | array:eval("values[]", asArray="squared[]", var="x", function={ squared := (x * x) }) | select(["squared[0]", "squared[1]"])
