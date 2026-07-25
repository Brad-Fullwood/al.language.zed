// Invalid: Quoted procedure name is fine, but missing begin keyword
codeunit 50204 "Bad Quoted Proc"
{
    procedure "Quoted Proc"(SomeDate: Date)
    var
        LocalDate: Date;
        LocalDate := 20201231D;
    end;
}
