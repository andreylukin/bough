"""Predicate pushdown."""


class Rewriter:
    def run(self, node):
        from .api import Filter, Join, Scan

        if isinstance(node, Scan):
            return node
        if isinstance(node, Join):
            return Join(self.run(node.left), self.run(node.right), node.on, node.kind)
        if isinstance(node, Filter):
            child = self.run(node.child)
            for part in self.split(node.pred):
                child = self.place(child, part)
            return child
        raise TypeError(node)

    def split(self, pred):
        """A conjunction splits into independently placeable parts."""
        if pred[0] == "and":
            return self.split(pred[1]) + self.split(pred[2])
        return [pred]

    def columns(self, node):
        from .api import Filter, Join, Scan

        if isinstance(node, Scan):
            return set(node.columns)
        if isinstance(node, Join):
            return self.columns(node.left) | self.columns(node.right)
        return self.columns(node.child)

    def refs(self, pred):
        if pred[0] in ("and", "or"):
            return self.refs(pred[1]) | self.refs(pred[2])
        return {pred[1]}

    def place(self, node, pred):
        """Put `pred` as deep as it will go."""
        from .api import Filter, Join

        if isinstance(node, Join):
            need = self.refs(pred)
            if need <= self.columns(node.left):
                return Join(self.place(node.left, pred), node.right, node.on, node.kind)
            if need <= self.columns(node.right):
                return Join(node.left, self.place(node.right, pred), node.on, node.kind)
        return Filter(node, pred)
