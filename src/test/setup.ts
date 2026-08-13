import "@testing-library/jest-dom";

// jsdom 没有 scrollIntoView 实现，组件里调用会抛 TypeError
Element.prototype.scrollIntoView = () => {};
