codeunit 50149 "AZJ Mutations"
{
    var
        GlobalItem: Record "azj journey item" temporary;

    procedure Mutate(NewQuantity: Decimal): Integer
    var
        JourneyItem: Record "AZJ Journey Item";
        Local1: Integer;
    begin
        Undeclared := 1;
        Local1 := Undeclared2 + 1;
        JourneyItem.NoSuchField := NewQuantity;
        JourneyItem."No Such Field" := NewQuantity;
        Local1 := JourneyItem.NoReadField;
        JourneyItem.NoSuchMethod();
        GlobalItem.NoSuchGlobalField := 1;
        Undeclared3.Foo();
        JourneyItem.Quantity := NewQuantity;
        JourneyItem."Customer No." := 'X';
        JourneyItem.SystemId := CreateGuid();
        JourneyItem.Insert(true);
        Local1 := Today() - Today + Local1;
        exit(Undeclared4 + 1);
    end;
}
