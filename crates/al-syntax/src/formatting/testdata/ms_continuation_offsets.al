codeunit 50100 "Probe Eight"
{
    procedure A()
    begin
        B.Setup()
                .Add(1)
         .Add(2)
                            .Add(3);
        if X then begin
            B.Setup()
                .Add(1)
                     .Add(2);
        end;
        B.Setup()
  .Add(1);
        B.Setup().Add(1)
        .Add(2);
        Foo(1,
 2,
         3);
        Foo(1,
    Bar()
    .Baz(),
 3);
    end;
}

